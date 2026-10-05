#if UNITY_EDITOR
using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using UnityEditor;
using UnityEngine;

namespace Polysquish.Editor
{
    /// <summary>
    /// Where the <c>polysquish</c> executable lives. The user-chosen path is stored in
    /// <see cref="EditorPrefs"/> (per user, not per project). Resolution order, shared by every
    /// Polysquish integration: <c>POLYSQUISH_BIN</c> environment variable, preferences, <c>PATH</c>,
    /// default install folders.
    /// </summary>
    public static class PolysquishSettings
    {
        public const string ReleasesUrl = "https://github.com/10v32/Polysquish/releases";
        public const string EnvVar = "POLYSQUISH_BIN";
        public const string AssetRoot = "Assets/Polysquish";
        public const string PreferencesPath = "Preferences/Polysquish";

        private const string ExecutableKey = "Polysquish.ExecutablePath";
        private const string LastInputKey = "Polysquish.LastInputPath";

        public static readonly string[] SupportedInputExtensions = { ".obj", ".ply", ".stl", ".glb", ".gltf" };

        public static string ExecutablePath
        {
            get { return EditorPrefs.GetString(ExecutableKey, string.Empty); }
            set { EditorPrefs.SetString(ExecutableKey, value ?? string.Empty); }
        }

        public static string LastInputPath
        {
            get { return EditorPrefs.GetString(LastInputKey, string.Empty); }
            set { EditorPrefs.SetString(LastInputKey, value ?? string.Empty); }
        }

        public static string ExecutableFileName
        {
            get { return Application.platform == RuntimePlatform.WindowsEditor ? "polysquish.exe" : "polysquish"; }
        }

        public static string MissingMessage
        {
            get
            {
                return "Polysquish executable not found.\n\nDownload it from " + ReleasesUrl +
                       " and set the path in Edit > Preferences > Polysquish, put it on PATH, or set the " +
                       EnvVar + " environment variable.";
            }
        }

        /// <summary>Resolve the executable. Returns null when nothing usable was found.</summary>
        public static string ResolveExecutable(out string source)
        {
            string env = Environment.GetEnvironmentVariable(EnvVar);
            if (IsUsable(env))
            {
                source = "environment variable " + EnvVar;
                return Path.GetFullPath(env.Trim());
            }

            string pref = ExecutablePath;
            if (IsUsable(pref))
            {
                source = "Preferences";
                return Path.GetFullPath(pref.Trim());
            }

            string onPath = FindOnPath();
            if (onPath != null)
            {
                source = "PATH";
                return onPath;
            }

            foreach (string candidate in DefaultLocations())
            {
                if (IsUsable(candidate))
                {
                    source = "default install folder";
                    return candidate;
                }
            }

            source = null;
            return null;
        }

        public static bool IsSupportedInput(string path)
        {
            if (string.IsNullOrEmpty(path)) return false;
            string ext = Path.GetExtension(path).ToLowerInvariant();
            return Array.IndexOf(SupportedInputExtensions, ext) >= 0;
        }

        /// <summary>Output of <c>polysquish --version</c>, or an empty string when it cannot be run.</summary>
        public static string QueryVersion(string executable)
        {
            if (!IsUsable(executable)) return string.Empty;
            try
            {
                var psi = new ProcessStartInfo
                {
                    FileName = executable,
                    Arguments = "--version",
                    UseShellExecute = false,
                    RedirectStandardOutput = true,
                    RedirectStandardError = true,
                    CreateNoWindow = true,
                };
                using (var process = Process.Start(psi))
                {
                    if (process == null) return string.Empty;
                    string output = process.StandardOutput.ReadToEnd();
                    string error = process.StandardError.ReadToEnd();
                    if (!process.WaitForExit(5000))
                    {
                        try { process.Kill(); } catch (InvalidOperationException) { }
                        return string.Empty;
                    }
                    string text = string.IsNullOrWhiteSpace(output) ? error : output;
                    return (text ?? string.Empty).Trim();
                }
            }
            catch (Exception)
            {
                return string.Empty;
            }
        }

        private static bool IsUsable(string path)
        {
            if (string.IsNullOrWhiteSpace(path)) return false;
            try
            {
                return File.Exists(path.Trim());
            }
            catch (Exception)
            {
                return false;
            }
        }

        private static string FindOnPath()
        {
            string pathVar = Environment.GetEnvironmentVariable("PATH") ?? string.Empty;
            foreach (string dir in pathVar.Split(Path.PathSeparator))
            {
                if (string.IsNullOrWhiteSpace(dir)) continue;
                try
                {
                    string candidate = Path.Combine(dir.Trim(), ExecutableFileName);
                    if (File.Exists(candidate)) return candidate;
                }
                catch (ArgumentException)
                {
                    // Malformed PATH entry; ignore it.
                }
            }
            return null;
        }

        public static IEnumerable<string> DefaultLocations()
        {
            string home = Environment.GetFolderPath(Environment.SpecialFolder.UserProfile);
            string exe = ExecutableFileName;
            var list = new List<string>();
            if (Application.platform == RuntimePlatform.WindowsEditor)
            {
                string[] roots =
                {
                    Environment.GetEnvironmentVariable("LOCALAPPDATA"),
                    Environment.GetEnvironmentVariable("ProgramFiles"),
                    Environment.GetEnvironmentVariable("ProgramFiles(x86)"),
                    home,
                };
                foreach (string root in roots)
                {
                    if (string.IsNullOrEmpty(root)) continue;
                    list.Add(Path.Combine(root, "Programs", "Polysquish", exe));
                    list.Add(Path.Combine(root, "Polysquish", exe));
                }
                list.Add(Path.Combine(home, "Downloads", exe));
                list.Add(Path.Combine(home, ".cargo", "bin", exe));
            }
            else if (Application.platform == RuntimePlatform.OSXEditor)
            {
                list.Add("/Applications/Polysquish/" + exe);
                list.Add(Path.Combine(home, "Applications", "Polysquish", exe));
                list.Add("/usr/local/bin/" + exe);
                list.Add("/opt/homebrew/bin/" + exe);
                list.Add(Path.Combine(home, ".local", "bin", exe));
                list.Add(Path.Combine(home, ".cargo", "bin", exe));
                list.Add(Path.Combine(home, "Downloads", exe));
            }
            else
            {
                list.Add("/usr/local/bin/" + exe);
                list.Add("/usr/bin/" + exe);
                list.Add(Path.Combine(home, ".local", "bin", exe));
                list.Add(Path.Combine(home, ".cargo", "bin", exe));
                list.Add("/opt/polysquish/" + exe);
                list.Add(Path.Combine(home, "Downloads", exe));
            }
            return list;
        }
    }

    /// <summary>Edit ▸ Preferences ▸ Polysquish.</summary>
    internal sealed class PolysquishSettingsProvider : SettingsProvider
    {
        private string _cachedVersionFor;
        private string _cachedVersion;

        private PolysquishSettingsProvider(string path, SettingsScope scope) : base(path, scope)
        {
            keywords = new HashSet<string>(new[] { "Polysquish", "mesh", "decimate", "LOD", "bake" });
        }

        [SettingsProvider]
        public static SettingsProvider Create()
        {
            return new PolysquishSettingsProvider(PolysquishSettings.PreferencesPath, SettingsScope.User);
        }

        public override void OnGUI(string searchContext)
        {
            EditorGUILayout.Space();
            EditorGUILayout.LabelField("Executable", EditorStyles.boldLabel);

            using (new EditorGUILayout.HorizontalScope())
            {
                string current = PolysquishSettings.ExecutablePath;
                string edited = EditorGUILayout.TextField("polysquish path", current);
                if (edited != current) PolysquishSettings.ExecutablePath = edited;

                if (GUILayout.Button("Browse…", GUILayout.Width(80f)))
                {
                    string picked = EditorUtility.OpenFilePanel("Locate the polysquish executable", string.Empty, string.Empty);
                    if (!string.IsNullOrEmpty(picked)) PolysquishSettings.ExecutablePath = picked;
                    GUIUtility.ExitGUI();
                }
            }

            string source;
            string resolved = PolysquishSettings.ResolveExecutable(out source);
            if (resolved != null)
            {
                if (_cachedVersionFor != resolved)
                {
                    _cachedVersionFor = resolved;
                    _cachedVersion = PolysquishSettings.QueryVersion(resolved);
                }
                EditorGUILayout.HelpBox("Using " + resolved + " (from " + source + ")\n" +
                                        (string.IsNullOrEmpty(_cachedVersion) ? "Version could not be read." : _cachedVersion),
                                        MessageType.Info);
            }
            else
            {
                EditorGUILayout.HelpBox(PolysquishSettings.MissingMessage, MessageType.Warning);
                if (GUILayout.Button("Open releases page", GUILayout.Width(160f)))
                {
                    Application.OpenURL(PolysquishSettings.ReleasesUrl);
                }
            }

            EditorGUILayout.Space();
            EditorGUILayout.LabelField("Lookup order: " + PolysquishSettings.EnvVar + " → this preference → PATH → default install folders.",
                EditorStyles.wordWrappedMiniLabel);
        }
    }
}
#endif
