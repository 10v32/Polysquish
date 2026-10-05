#if UNITY_EDITOR
using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Text.RegularExpressions;
using UnityEditor;
using UnityEngine;
using UnityEngine.Rendering;

namespace Polysquish.Editor
{
    /// <summary>
    /// Window ▸ Polysquish. Picks a model file, runs <c>polysquish squish --target unity</c> in the
    /// background, copies the outputs into <c>Assets/Polysquish/&lt;name&gt;/</c>, fixes import settings,
    /// builds a material from the baked textures and a <see cref="LODGroup"/> prefab.
    /// </summary>
    public sealed class PolysquishWindow : EditorWindow
    {
        private static readonly string[] PresetIds = { "hero", "prop", "mobile", "character", "dcc" };
        private static readonly string[] PresetLabels =
        {
            "Hero asset (30k, 2K, 3 LODs)",
            "Environment prop (5k, 1K, 3 LODs)",
            "Mobile / VR (1.5k, 1K, 2 LODs)",
            "Character (40k, quad-dominant, rig kept)",
            "Clean for DCC (150k, 4K, no LODs)",
        };
        private static readonly int[] TextureSizes = { 0, 512, 1024, 2048, 4096, 8192 };
        private static readonly string[] TextureLabels = { "Preset default", "512", "1K", "2K", "4K", "8K" };
        private static readonly string[] TargetIds = { "unity", "generic", "godot", "unreal" };
        private static readonly string[] TargetLabels = { "Unity", "Generic", "Godot", "Unreal" };
        /// <summary>LOD transition screen heights for LOD0, LOD1, LOD2, LOD3 (the last one is the cull height).</summary>
        private static readonly float[] ScreenHeights = { 0.5f, 0.25f, 0.1f, 0.01f };

        [SerializeField] private string _inputPath = string.Empty;
        [SerializeField] private string _assetName = string.Empty;
        [SerializeField] private int _presetIndex;
        [SerializeField] private bool _overrideBudget;
        [SerializeField] private int _targetTris = 30000;
        [SerializeField] private int _textureIndex;
        [SerializeField] private int _targetIndex;
        [SerializeField] private bool _bakeTextures = true;
        [SerializeField] private bool _bakeAo = true;
        [SerializeField] private bool _buildLodPrefab = true;
        [SerializeField] private bool _importCollision = true;
        [SerializeField] private Vector2 _scroll;

        private PolysquishJob _job;
        private string _tempDir;
        private string _outDir;
        private string _jobName;
        private string _lastSummary = string.Empty;
        private string _lastPrefabPath = string.Empty;
        private string _lastFolder = string.Empty;
        private string _lastReportPath = string.Empty;

        [MenuItem("Window/Polysquish")]
        public static void Open()
        {
            var window = GetWindow<PolysquishWindow>("Polysquish");
            window.minSize = new Vector2(360f, 420f);
            window.Show();
        }

        private void OnEnable()
        {
            if (string.IsNullOrEmpty(_inputPath)) _inputPath = PolysquishSettings.LastInputPath;
        }

        private void OnDisable()
        {
            if (_job != null && !_job.IsDone)
            {
                _job.Cancel();
                _job.Dispose();
                _job = null;
                CleanupTemp();
            }
        }

        private void OnInspectorUpdate()
        {
            if (_job != null) Repaint();
        }

        // ------------------------------------------------------------------ GUI

        private void OnGUI()
        {
            _scroll = EditorGUILayout.BeginScrollView(_scroll);
            DrawExecutable();
            EditorGUILayout.Space();
            DrawInput();
            EditorGUILayout.Space();
            DrawSettings();
            EditorGUILayout.Space();
            DrawRun();
            EditorGUILayout.EndScrollView();
        }

        private void DrawExecutable()
        {
            string source;
            string exe = PolysquishSettings.ResolveExecutable(out source);
            if (exe != null)
            {
                EditorGUILayout.LabelField("Executable", Path.GetFileName(exe) + "  (" + source + ")", EditorStyles.miniLabel);
                return;
            }
            EditorGUILayout.HelpBox(PolysquishSettings.MissingMessage, MessageType.Error);
            using (new EditorGUILayout.HorizontalScope())
            {
                if (GUILayout.Button("Download…")) Application.OpenURL(PolysquishSettings.ReleasesUrl);
                if (GUILayout.Button("Preferences…")) SettingsService.OpenUserPreferences(PolysquishSettings.PreferencesPath);
            }
        }

        private void DrawInput()
        {
            EditorGUILayout.LabelField("Input", EditorStyles.boldLabel);
            using (new EditorGUILayout.HorizontalScope())
            {
                string edited = EditorGUILayout.TextField("Model file", _inputPath);
                if (edited != _inputPath) SetInput(edited);
                if (GUILayout.Button("Browse…", GUILayout.Width(70f)))
                {
                    string dir = string.IsNullOrEmpty(_inputPath) ? string.Empty : Path.GetDirectoryName(_inputPath);
                    string picked = EditorUtility.OpenFilePanelWithFilters("Model to squish", dir ?? string.Empty,
                        new[] { "3D models", "obj,ply,stl,glb,gltf", "All files", "*" });
                    if (!string.IsNullOrEmpty(picked)) SetInput(picked);
                    GUIUtility.ExitGUI();
                }
            }

            string selectedPath = SelectedModelAssetPath();
            using (new EditorGUI.DisabledScope(selectedPath == null))
            {
                string label = selectedPath == null ? "Use selected asset (select a model in the Project window)" : "Use selected asset: " + Path.GetFileName(selectedPath);
                if (GUILayout.Button(label)) SetInput(Path.GetFullPath(selectedPath));
            }

            _assetName = EditorGUILayout.TextField("Asset name", _assetName);
            if (!string.IsNullOrEmpty(_inputPath) && !File.Exists(_inputPath))
            {
                EditorGUILayout.HelpBox("File not found.", MessageType.Warning);
            }
            else if (!string.IsNullOrEmpty(_inputPath) && !PolysquishSettings.IsSupportedInput(_inputPath))
            {
                EditorGUILayout.HelpBox("Unsupported file type. polysquish reads .obj .ply .stl .glb .gltf", MessageType.Warning);
            }
        }

        private void DrawSettings()
        {
            EditorGUILayout.LabelField("Squish", EditorStyles.boldLabel);
            _presetIndex = EditorGUILayout.Popup("Preset", _presetIndex, PresetLabels);
            using (new EditorGUILayout.HorizontalScope())
            {
                _overrideBudget = EditorGUILayout.ToggleLeft("Triangle budget", _overrideBudget, GUILayout.Width(EditorGUIUtility.labelWidth));
                using (new EditorGUI.DisabledScope(!_overrideBudget))
                {
                    _targetTris = Mathf.Max(50, EditorGUILayout.IntField(_targetTris));
                }
            }
            _textureIndex = EditorGUILayout.Popup("Texture size", _textureIndex, TextureLabels);
            _targetIndex = EditorGUILayout.Popup("Engine target", _targetIndex, TargetLabels);
            _bakeTextures = EditorGUILayout.Toggle("Bake textures", _bakeTextures);
            using (new EditorGUI.DisabledScope(!_bakeTextures))
            {
                _bakeAo = EditorGUILayout.Toggle("    Ambient occlusion", _bakeAo);
            }
            EditorGUILayout.Space();
            EditorGUILayout.LabelField("Import", EditorStyles.boldLabel);
            _buildLodPrefab = EditorGUILayout.Toggle("Build LODGroup prefab", _buildLodPrefab);
            _importCollision = EditorGUILayout.Toggle("Add convex collider", _importCollision);
            EditorGUILayout.LabelField("Files go to " + PolysquishSettings.AssetRoot + "/<name>/", EditorStyles.miniLabel);
        }

        private void DrawRun()
        {
            string source;
            bool haveExe = PolysquishSettings.ResolveExecutable(out source) != null;
            bool haveInput = !string.IsNullOrEmpty(_inputPath) && File.Exists(_inputPath) && PolysquishSettings.IsSupportedInput(_inputPath);

            if (_job != null && !_job.IsDone)
            {
                Rect rect = EditorGUILayout.GetControlRect(false, 20f);
                EditorGUI.ProgressBar(rect, _job.Progress, _job.Stage + "  (" + _job.ElapsedSeconds.ToString("0") + "s)");
                if (GUILayout.Button("Cancel")) _job.Cancel();
                return;
            }

            using (new EditorGUI.DisabledScope(!haveExe || !haveInput))
            {
                if (GUILayout.Button("Squish", GUILayout.Height(32f))) StartSquish();
            }

            if (!string.IsNullOrEmpty(_lastSummary))
            {
                EditorGUILayout.Space();
                EditorGUILayout.HelpBox(_lastSummary, MessageType.Info);
                using (new EditorGUILayout.HorizontalScope())
                {
                    if (!string.IsNullOrEmpty(_lastPrefabPath) && GUILayout.Button("Select prefab"))
                    {
                        var prefab = AssetDatabase.LoadAssetAtPath<GameObject>(_lastPrefabPath);
                        if (prefab != null)
                        {
                            Selection.activeObject = prefab;
                            EditorGUIUtility.PingObject(prefab);
                        }
                    }
                    if (!string.IsNullOrEmpty(_lastReportPath) && File.Exists(_lastReportPath) && GUILayout.Button("Open report"))
                    {
                        Application.OpenURL("file://" + _lastReportPath.Replace('\\', '/'));
                    }
                    if (!string.IsNullOrEmpty(_lastFolder) && GUILayout.Button("Show folder"))
                    {
                        var folder = AssetDatabase.LoadAssetAtPath<UnityEngine.Object>(_lastFolder);
                        if (folder != null) EditorGUIUtility.PingObject(folder);
                    }
                }
            }
        }

        private void SetInput(string path)
        {
            _inputPath = path ?? string.Empty;
            PolysquishSettings.LastInputPath = _inputPath;
            if (string.IsNullOrEmpty(_assetName) && !string.IsNullOrEmpty(_inputPath))
            {
                _assetName = Sanitize(Path.GetFileNameWithoutExtension(_inputPath));
            }
        }

        private static string SelectedModelAssetPath()
        {
            var obj = Selection.activeObject;
            if (obj == null) return null;
            string path = AssetDatabase.GetAssetPath(obj);
            return PolysquishSettings.IsSupportedInput(path) ? path : null;
        }

        // ------------------------------------------------------------------ run

        private void StartSquish()
        {
            string source;
            string exe = PolysquishSettings.ResolveExecutable(out source);
            if (exe == null)
            {
                EditorUtility.DisplayDialog("Polysquish", PolysquishSettings.MissingMessage, "OK");
                return;
            }
            string input = Path.GetFullPath(_inputPath);
            if (!File.Exists(input))
            {
                EditorUtility.DisplayDialog("Polysquish", "File not found:\n" + input, "OK");
                return;
            }

            _jobName = Sanitize(string.IsNullOrWhiteSpace(_assetName) ? Path.GetFileNameWithoutExtension(input) : _assetName);
            _tempDir = Path.Combine(Path.GetTempPath(), "polysquish", Guid.NewGuid().ToString("N"));
            _outDir = Path.Combine(_tempDir, _jobName + "_squished");
            Directory.CreateDirectory(_tempDir);

            var args = new List<string>
            {
                "squish", input,
                "--preset", PresetIds[_presetIndex],
                "--target", TargetIds[_targetIndex],
                "-o", _outDir,
                "--name", _jobName,
            };
            if (_overrideBudget) { args.Add("--target-tris"); args.Add(_targetTris.ToString()); }
            if (TextureSizes[_textureIndex] > 0) { args.Add("--texture"); args.Add(TextureSizes[_textureIndex].ToString()); }
            if (!_bakeTextures) args.Add("--no-bake");
            else if (!_bakeAo) args.Add("--no-ao");

            try
            {
                _job = PolysquishJob.Start(exe, args, _tempDir);
            }
            catch (Exception e)
            {
                CleanupTemp();
                EditorUtility.DisplayDialog("Polysquish", "Could not start " + exe + ":\n" + e.Message, "OK");
                return;
            }
            Debug.Log("[Polysquish] " + _job.CommandLine);
            _lastSummary = string.Empty;
            _job.Completed += OnJobCompleted;
        }

        private void OnJobCompleted(PolysquishJob job)
        {
            try
            {
                if (job.WasCancelled)
                {
                    _lastSummary = "Cancelled.";
                    return;
                }
                if (job.ExitCode != 0)
                {
                    Debug.LogError("[Polysquish] exit code " + job.ExitCode + "\n" + job.Output);
                    _lastSummary = "polysquish failed (exit " + job.ExitCode + "). See the Console.";
                    EditorUtility.DisplayDialog("Polysquish failed", "Exit code " + job.ExitCode + "\n\n" + job.Tail(12), "OK");
                    return;
                }
                ImportResults(_outDir, _jobName, job.ElapsedSeconds);
            }
            catch (Exception e)
            {
                Debug.LogException(e);
                _lastSummary = "Import failed: " + e.Message;
                EditorUtility.DisplayDialog("Polysquish", "Importing the result failed:\n" + e.Message, "OK");
            }
            finally
            {
                job.Dispose();
                if (_job == job) _job = null;
                CleanupTemp();
                Repaint();
            }
        }

        private void CleanupTemp()
        {
            if (string.IsNullOrEmpty(_tempDir)) return;
            try
            {
                if (Directory.Exists(_tempDir)) Directory.Delete(_tempDir, true);
            }
            catch (Exception e)
            {
                Debug.LogWarning("[Polysquish] could not remove " + _tempDir + ": " + e.Message);
            }
            _tempDir = null;
        }

        // ------------------------------------------------------------------ import

        private void ImportResults(string outDir, string name, double elapsed)
        {
            if (!Directory.Exists(outDir)) throw new DirectoryNotFoundException("polysquish produced no output folder: " + outDir);

            string destFolder = PolysquishSettings.AssetRoot + "/" + name;
            EnsureFolder(destFolder);

            var copied = new List<string>();
            AssetDatabase.StartAssetEditing();
            try
            {
                foreach (string file in Directory.GetFiles(outDir))
                {
                    string fileName = Path.GetFileName(file);
                    string dest = destFolder + "/" + fileName;
                    File.Copy(file, Path.GetFullPath(dest), true);
                    copied.Add(dest);
                }
            }
            finally
            {
                AssetDatabase.StopAssetEditing();
            }
            foreach (string asset in copied) AssetDatabase.ImportAsset(asset, ImportAssetOptions.ForceUpdate);
            AssetDatabase.Refresh();

            // Import settings (the postprocessor already applied them on import; this catches anything it missed).
            foreach (string asset in copied)
            {
                string ext = Path.GetExtension(asset).ToLowerInvariant();
                if (ext == ".png")
                {
                    var ti = AssetImporter.GetAtPath(asset) as TextureImporter;
                    if (PolysquishImportSettings.ApplyTexture(ti, asset)) ti.SaveAndReimport();
                }
                else if (ext == ".obj" || ext == ".fbx")
                {
                    var mi = AssetImporter.GetAtPath(asset) as ModelImporter;
                    if (PolysquishImportSettings.ApplyModel(mi, asset)) mi.SaveAndReimport();
                }
            }

            ResultJson result = ReadResult(Path.Combine(outDir, "result.json"));
            Material material = BuildMaterial(destFolder, name);
            string prefabPath = _buildLodPrefab ? BuildPrefab(destFolder, name, material) : null;

            AssetDatabase.SaveAssets();

            _lastFolder = destFolder;
            _lastPrefabPath = prefabPath ?? string.Empty;
            _lastReportPath = Path.GetFullPath(destFolder + "/report.html");
            _lastSummary = Summarize(result, elapsed, prefabPath);
            Debug.Log("[Polysquish] " + _lastSummary.Replace('\n', ' '));

            if (!string.IsNullOrEmpty(prefabPath))
            {
                var prefab = AssetDatabase.LoadAssetAtPath<GameObject>(prefabPath);
                if (prefab != null)
                {
                    Selection.activeObject = prefab;
                    EditorGUIUtility.PingObject(prefab);
                }
            }
        }

        private static void EnsureFolder(string assetFolder)
        {
            string[] parts = assetFolder.Split('/');
            string current = parts[0];
            for (int i = 1; i < parts.Length; i++)
            {
                string next = current + "/" + parts[i];
                if (!AssetDatabase.IsValidFolder(next)) AssetDatabase.CreateFolder(current, parts[i]);
                current = next;
            }
        }

        private static Material BuildMaterial(string destFolder, string name)
        {
            var albedo = AssetDatabase.LoadAssetAtPath<Texture2D>(destFolder + "/" + name + "_albedo.png");
            var normal = AssetDatabase.LoadAssetAtPath<Texture2D>(destFolder + "/" + name + "_normal.png");
            var ao = AssetDatabase.LoadAssetAtPath<Texture2D>(destFolder + "/" + name + "_ao.png");
            if (albedo == null && normal == null && ao == null) return null;

            Shader shader = null;
            if (GraphicsSettings.currentRenderPipeline != null)
            {
                shader = Shader.Find("Universal Render Pipeline/Lit") ?? Shader.Find("HDRP/Lit");
            }
            if (shader == null) shader = Shader.Find("Standard");
            if (shader == null) return null;

            string matPath = destFolder + "/" + name + "_material.mat";
            var material = AssetDatabase.LoadAssetAtPath<Material>(matPath);
            if (material == null)
            {
                material = new Material(shader);
                AssetDatabase.CreateAsset(material, matPath);
            }
            else
            {
                material.shader = shader;
            }

            if (albedo != null)
            {
                if (material.HasProperty("_BaseMap")) material.SetTexture("_BaseMap", albedo);
                if (material.HasProperty("_BaseColorMap")) material.SetTexture("_BaseColorMap", albedo);
                if (material.HasProperty("_MainTex")) material.SetTexture("_MainTex", albedo);
                if (material.HasProperty("_BaseColor")) material.SetColor("_BaseColor", Color.white);
                if (material.HasProperty("_Color")) material.SetColor("_Color", Color.white);
            }
            if (normal != null)
            {
                if (material.HasProperty("_BumpMap")) { material.SetTexture("_BumpMap", normal); material.EnableKeyword("_NORMALMAP"); }
                if (material.HasProperty("_NormalMap")) { material.SetTexture("_NormalMap", normal); material.EnableKeyword("_NORMALMAP_TANGENT_SPACE"); }
            }
            if (ao != null && material.HasProperty("_OcclusionMap"))
            {
                // Built-in and URP read occlusion from the G channel; the baked AO is grey so that works.
                material.SetTexture("_OcclusionMap", ao);
                material.EnableKeyword("_OCCLUSIONMAP");
            }
            EditorUtility.SetDirty(material);
            return material;
        }

        /// <summary>
        /// Builds <c>&lt;dest&gt;/&lt;name&gt;.prefab</c> with a <see cref="LODGroup"/>. LOD sources come from the
        /// GLB when the glTF importer in use exposes the MSFT_lod nodes (children named *_LOD1, *_LOD2…),
        /// otherwise from the per-LOD OBJ files written by polysquish.
        /// </summary>
        private string BuildPrefab(string destFolder, string name, Material material)
        {
            var lodRenderers = new List<List<Renderer>>();
            var root = new GameObject(name);
            try
            {
                var glb = AssetDatabase.LoadAssetAtPath<GameObject>(destFolder + "/" + name + ".glb");
                var obj0 = AssetDatabase.LoadAssetAtPath<GameObject>(destFolder + "/" + name + ".obj");
                var fbx = AssetDatabase.LoadAssetAtPath<GameObject>(destFolder + "/" + name + ".fbx");

                bool glbHasLods = false;
                if (glb != null)
                {
                    var instance = PrefabUtility.InstantiatePrefab(glb) as GameObject;
                    if (instance != null)
                    {
                        instance.transform.SetParent(root.transform, false);
                        var byLevel = GroupRenderersByLodSuffix(instance);
                        if (byLevel.Count > 1)
                        {
                            glbHasLods = true;
                            foreach (var level in byLevel.Keys.OrderBy(k => k)) lodRenderers.Add(byLevel[level]);
                        }
                        else if (obj0 == null && fbx == null)
                        {
                            lodRenderers.Add(instance.GetComponentsInChildren<Renderer>(true).ToList());
                        }
                        else
                        {
                            DestroyImmediate(instance);
                        }
                    }
                }

                if (!glbHasLods && lodRenderers.Count == 0)
                {
                    var sources = new List<GameObject>();
                    GameObject lod0 = obj0 ?? fbx;
                    if (lod0 != null) sources.Add(lod0);
                    for (int i = 1; i < 16; i++)
                    {
                        var lod = AssetDatabase.LoadAssetAtPath<GameObject>(destFolder + "/" + name + "_LOD" + i + ".obj");
                        if (lod == null) break;
                        sources.Add(lod);
                    }
                    for (int i = 0; i < sources.Count; i++)
                    {
                        var instance = PrefabUtility.InstantiatePrefab(sources[i]) as GameObject;
                        if (instance == null) continue;
                        instance.name = name + "_LOD" + i;
                        instance.transform.SetParent(root.transform, false);
                        var renderers = instance.GetComponentsInChildren<Renderer>(true);
                        if (material != null)
                        {
                            foreach (var r in renderers)
                            {
                                r.sharedMaterials = Enumerable.Repeat(material, Math.Max(1, r.sharedMaterials.Length)).ToArray();
                            }
                        }
                        lodRenderers.Add(renderers.ToList());
                    }
                }

                if (lodRenderers.Count == 0)
                {
                    Debug.LogWarning("[Polysquish] No importable mesh found in " + destFolder +
                                     ". Install a glTF importer (e.g. com.unity.cloud.gltfast) or keep OBJ export enabled.");
                    return null;
                }

                if (lodRenderers.Count > 1)
                {
                    var lodGroup = root.AddComponent<LODGroup>();
                    var lods = new LOD[lodRenderers.Count];
                    float previous = 1f;
                    for (int i = 0; i < lods.Length; i++)
                    {
                        float height = i < ScreenHeights.Length ? ScreenHeights[i] : previous * 0.5f;
                        if (height >= previous) height = previous * 0.5f;
                        lods[i] = new LOD(height, lodRenderers[i].ToArray());
                        previous = height;
                    }
                    lodGroup.SetLODs(lods);
                    lodGroup.RecalculateBounds();
                }

                if (_importCollision) AddCollision(root, destFolder, name);

                string prefabPath = destFolder + "/" + name + ".prefab";
                PrefabUtility.SaveAsPrefabAsset(root, prefabPath);
                return prefabPath;
            }
            finally
            {
                DestroyImmediate(root);
            }
        }

        private static Dictionary<int, List<Renderer>> GroupRenderersByLodSuffix(GameObject instance)
        {
            var regex = new Regex(@"_LOD(\d+)$", RegexOptions.IgnoreCase);
            var result = new Dictionary<int, List<Renderer>>();
            foreach (var renderer in instance.GetComponentsInChildren<Renderer>(true))
            {
                int level = 0;
                Transform t = renderer.transform;
                while (t != null && t != instance.transform.parent)
                {
                    var m = regex.Match(t.name);
                    if (m.Success)
                    {
                        level = int.Parse(m.Groups[1].Value);
                        break;
                    }
                    t = t.parent;
                }
                List<Renderer> list;
                if (!result.TryGetValue(level, out list))
                {
                    list = new List<Renderer>();
                    result[level] = list;
                }
                list.Add(renderer);
            }
            return result;
        }

        private static void AddCollision(GameObject root, string destFolder, string name)
        {
            var hull = AssetDatabase.LoadAssetAtPath<GameObject>(destFolder + "/" + name + "_collision_hull.obj");
            if (hull == null) return;
            var instance = PrefabUtility.InstantiatePrefab(hull) as GameObject;
            if (instance == null) return;
            // The nested prefab instance must be unpacked before its components can be removed.
            PrefabUtility.UnpackPrefabInstance(instance, PrefabUnpackMode.Completely, InteractionMode.AutomatedAction);
            instance.name = "Collision";
            instance.transform.SetParent(root.transform, false);
            foreach (var filter in instance.GetComponentsInChildren<MeshFilter>(true))
            {
                var go = filter.gameObject;
                var collider = go.GetComponent<MeshCollider>();
                if (collider == null) collider = go.AddComponent<MeshCollider>();
                collider.sharedMesh = filter.sharedMesh;
                collider.convex = true;
                var renderer = go.GetComponent<MeshRenderer>();
                if (renderer != null) DestroyImmediate(renderer);
                DestroyImmediate(filter);
            }
        }

        // ------------------------------------------------------------------ result.json

        [Serializable]
        private class Stats
        {
            public long triangles;
            public long vertices;
            public int texture_size;
        }

        [Serializable]
        private class ResultJson
        {
            public string name;
            public Stats before;
            public Stats after;
            public string main_glb;
            public string main_obj;
            [NonSerialized] public List<KeyValuePair<string, double>> timings = new List<KeyValuePair<string, double>>();
        }

        private static ResultJson ReadResult(string path)
        {
            if (!File.Exists(path)) return null;
            try
            {
                string json = File.ReadAllText(path);
                var result = JsonUtility.FromJson<ResultJson>(json) ?? new ResultJson();
                result.timings = new List<KeyValuePair<string, double>>();
                // JsonUtility cannot read dictionaries; pull the timings map out with a regex.
                var block = Regex.Match(json, "\"timings\"\\s*:\\s*\\{([^}]*)\\}");
                if (block.Success)
                {
                    foreach (Match m in Regex.Matches(block.Groups[1].Value, "\"(\\w+)\"\\s*:\\s*([0-9.eE+-]+)"))
                    {
                        double seconds;
                        if (double.TryParse(m.Groups[2].Value, System.Globalization.NumberStyles.Float,
                                System.Globalization.CultureInfo.InvariantCulture, out seconds))
                        {
                            result.timings.Add(new KeyValuePair<string, double>(m.Groups[1].Value, seconds));
                        }
                    }
                }
                return result;
            }
            catch (Exception e)
            {
                Debug.LogWarning("[Polysquish] could not read result.json: " + e.Message);
                return null;
            }
        }

        private static string Summarize(ResultJson result, double elapsed, string prefabPath)
        {
            string text;
            if (result != null && result.before != null && result.after != null)
            {
                text = "Squished " + result.before.triangles.ToString("N0") + " → " + result.after.triangles.ToString("N0") +
                       " triangles in " + elapsed.ToString("0.0") + "s";
                if (result.timings.Count > 0)
                {
                    text += "\n" + string.Join(", ", result.timings.Select(kv => kv.Key + " " + kv.Value.ToString("0.0") + "s").ToArray());
                }
            }
            else
            {
                text = "Done in " + elapsed.ToString("0.0") + "s";
            }
            if (!string.IsNullOrEmpty(prefabPath)) text += "\nPrefab: " + prefabPath;
            return text;
        }

        private static string Sanitize(string name)
        {
            string clean = Regex.Replace(name ?? string.Empty, @"[^\w\-]+", "_").Trim('_');
            return string.IsNullOrEmpty(clean) ? "model" : clean;
        }
    }
}
#endif
