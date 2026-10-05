#if UNITY_EDITOR
using System;
using System.Collections.Concurrent;
using System.Collections.Generic;
using System.Diagnostics;
using System.Text;
using UnityEditor;

namespace Polysquish.Editor
{
    /// <summary>
    /// A running <c>polysquish squish</c> process. Output is captured on background threads and
    /// drained on <see cref="EditorApplication.update"/>, so the editor never blocks. Progress is
    /// derived from the stage markers the CLI prints to stderr ("▶ label" / "✓ label").
    /// </summary>
    public sealed class PolysquishJob : IDisposable
    {
        public static readonly string[] StageLabels =
        {
            "Reading model", "Health check", "Cleaning", "Squishing polygons", "Unwrapping UVs",
            "Baking textures", "Building LODs", "Collision shapes", "Exporting",
        };

        private const string StageStart = "▶"; // ▶
        private const string StageDone = "✓";  // ✓

        private readonly ConcurrentQueue<string> _queue = new ConcurrentQueue<string>();
        private readonly List<string> _lines = new List<string>();
        private readonly Stopwatch _stopwatch = new Stopwatch();
        private Process _process;
        private int _stagesDone;
        private bool _completedRaised;

        public string CommandLine { get; private set; }
        public string Stage { get; private set; }
        public bool IsDone { get; private set; }
        public bool WasCancelled { get; private set; }
        public int ExitCode { get; private set; }
        public double ElapsedSeconds { get { return _stopwatch.Elapsed.TotalSeconds; } }
        public float Progress { get { return Math.Min(1f, _stagesDone / (float)StageLabels.Length); } }
        public string Output { get { return string.Join("\n", _lines.ToArray()); } }

        /// <summary>Raised once, on the main thread, when the process has exited.</summary>
        public event Action<PolysquishJob> Completed;

        private PolysquishJob() { }

        public static PolysquishJob Start(string executable, IList<string> arguments, string workingDirectory)
        {
            var job = new PolysquishJob();
            var psi = new ProcessStartInfo
            {
                FileName = executable,
                Arguments = JoinArguments(arguments),
                UseShellExecute = false,
                RedirectStandardOutput = true,
                RedirectStandardError = true,
                RedirectStandardInput = false,
                CreateNoWindow = true,
                WorkingDirectory = workingDirectory ?? string.Empty,
                StandardOutputEncoding = Encoding.UTF8,
                StandardErrorEncoding = Encoding.UTF8,
            };
            job.CommandLine = Quote(executable) + " " + psi.Arguments;
            job.Stage = "Starting polysquish";

            var process = new Process { StartInfo = psi, EnableRaisingEvents = false };
            process.OutputDataReceived += (sender, e) => { if (e.Data != null) job._queue.Enqueue(e.Data); };
            process.ErrorDataReceived += (sender, e) => { if (e.Data != null) job._queue.Enqueue(e.Data); };
            job._process = process;

            process.Start(); // throws Win32Exception when the file cannot be executed
            process.BeginOutputReadLine();
            process.BeginErrorReadLine();
            job._stopwatch.Start();
            EditorApplication.update += job.Poll;
            return job;
        }

        /// <summary>The last <paramref name="count"/> informative output lines, for error messages.</summary>
        public string Tail(int count)
        {
            var interesting = new List<string>();
            foreach (string line in _lines)
            {
                string t = line.Trim();
                if (t.Length == 0 || t.StartsWith(StageStart, StringComparison.Ordinal) || t.StartsWith(StageDone, StringComparison.Ordinal)) continue;
                interesting.Add(t);
            }
            if (interesting.Count == 0) return "(no output)";
            int start = Math.Max(0, interesting.Count - count);
            return string.Join("\n", interesting.GetRange(start, interesting.Count - start).ToArray());
        }

        public void Cancel()
        {
            if (IsDone || _process == null) return;
            WasCancelled = true;
            try
            {
                if (!_process.HasExited) _process.Kill();
            }
            catch (InvalidOperationException) { }
            catch (System.ComponentModel.Win32Exception) { }
        }

        public void Dispose()
        {
            EditorApplication.update -= Poll;
            if (_process != null)
            {
                try
                {
                    if (!_process.HasExited) _process.Kill();
                }
                catch (InvalidOperationException) { }
                catch (System.ComponentModel.Win32Exception) { }
                _process.Dispose();
                _process = null;
            }
        }

        private void Poll()
        {
            Drain();
            if (_process == null || !_process.HasExited) return;

            // WaitForExit() without a timeout also flushes the asynchronous readers.
            try { _process.WaitForExit(); } catch (InvalidOperationException) { }
            Drain();
            ExitCode = _process.ExitCode;
            IsDone = true;
            _stopwatch.Stop();
            EditorApplication.update -= Poll;
            if (!_completedRaised)
            {
                _completedRaised = true;
                var handler = Completed;
                if (handler != null) handler(this);
            }
        }

        private void Drain()
        {
            string line;
            while (_queue.TryDequeue(out line))
            {
                _lines.Add(line);
                string t = line.Trim();
                if (t.StartsWith(StageStart, StringComparison.Ordinal))
                {
                    string label = t.Substring(StageStart.Length).Trim();
                    if (Array.IndexOf(StageLabels, label) >= 0) Stage = label;
                }
                else if (t.StartsWith(StageDone, StringComparison.Ordinal))
                {
                    string label = t.Substring(StageDone.Length).Trim();
                    if (Array.IndexOf(StageLabels, label) >= 0) _stagesDone++;
                }
                else if (t.Length > 0 && !t.StartsWith("Done in", StringComparison.Ordinal))
                {
                    Stage = t.Length > 80 ? t.Substring(0, 80) : t;
                }
            }
        }

        // ---- argument quoting (ProcessStartInfo.ArgumentList is not available on every Unity API profile)

        public static string JoinArguments(IList<string> arguments)
        {
            var sb = new StringBuilder();
            for (int i = 0; i < arguments.Count; i++)
            {
                if (i > 0) sb.Append(' ');
                sb.Append(Quote(arguments[i]));
            }
            return sb.ToString();
        }

        /// <summary>Quote one argument following the MSVC/CommandLineToArgvW rules (also fine for POSIX execvp).</summary>
        public static string Quote(string argument)
        {
            if (string.IsNullOrEmpty(argument)) return "\"\"";
            bool needsQuotes = false;
            foreach (char c in argument)
            {
                if (char.IsWhiteSpace(c) || c == '"')
                {
                    needsQuotes = true;
                    break;
                }
            }
            if (!needsQuotes) return argument;

            var sb = new StringBuilder();
            sb.Append('"');
            int backslashes = 0;
            foreach (char c in argument)
            {
                if (c == '\\')
                {
                    backslashes++;
                    continue;
                }
                if (c == '"')
                {
                    sb.Append('\\', backslashes * 2 + 1);
                    sb.Append('"');
                }
                else
                {
                    sb.Append('\\', backslashes);
                    sb.Append(c);
                }
                backslashes = 0;
            }
            sb.Append('\\', backslashes * 2);
            sb.Append('"');
            return sb.ToString();
        }
    }
}
#endif
