#if UNITY_EDITOR
using System;
using System.IO;
using UnityEditor;

namespace Polysquish.Editor
{
    /// <summary>
    /// Import settings for files Polysquish writes. Used both by the <see cref="AssetPostprocessor"/>
    /// below (so settings are right on first import) and by the window after copying files.
    /// </summary>
    public static class PolysquishImportSettings
    {
        public static bool IsPolysquishAsset(string assetPath)
        {
            return !string.IsNullOrEmpty(assetPath) &&
                   assetPath.Replace('\\', '/').StartsWith(PolysquishSettings.AssetRoot + "/", StringComparison.OrdinalIgnoreCase);
        }

        public static bool IsNormalMap(string path) { return Stem(path).EndsWith("_normal", StringComparison.Ordinal); }
        public static bool IsLinearMask(string path)
        {
            string stem = Stem(path);
            return stem.EndsWith("_orm", StringComparison.Ordinal) || stem.EndsWith("_ao", StringComparison.Ordinal);
        }
        public static bool IsAlbedo(string path) { return Stem(path).EndsWith("_albedo", StringComparison.Ordinal); }
        public static bool IsCollisionMesh(string path) { return Stem(path).Contains("_collision_"); }

        /// <summary>Apply texture settings. Returns true when something was changed.</summary>
        public static bool ApplyTexture(TextureImporter importer, string assetPath)
        {
            if (importer == null) return false;
            bool changed = false;
            if (IsNormalMap(assetPath))
            {
                if (importer.textureType != TextureImporterType.NormalMap) { importer.textureType = TextureImporterType.NormalMap; changed = true; }
                if (importer.sRGBTexture) { importer.sRGBTexture = false; changed = true; }
            }
            else if (IsLinearMask(assetPath))
            {
                if (importer.textureType != TextureImporterType.Default) { importer.textureType = TextureImporterType.Default; changed = true; }
                if (importer.sRGBTexture) { importer.sRGBTexture = false; changed = true; }
            }
            else if (IsAlbedo(assetPath))
            {
                if (importer.textureType != TextureImporterType.Default) { importer.textureType = TextureImporterType.Default; changed = true; }
                if (!importer.sRGBTexture) { importer.sRGBTexture = true; changed = true; }
            }
            return changed;
        }

        /// <summary>Apply model (OBJ/FBX) settings. Returns true when something was changed.</summary>
        public static bool ApplyModel(ModelImporter importer, string assetPath)
        {
            if (importer == null) return false;
            bool changed = false;
            // Materials are rebuilt by the window from the baked textures; skip the MTL-derived ones.
            if (importer.materialImportMode != ModelImporterMaterialImportMode.None)
            {
                importer.materialImportMode = ModelImporterMaterialImportMode.None;
                changed = true;
            }
            if (importer.generateSecondaryUV) { importer.generateSecondaryUV = false; changed = true; }
            if (IsCollisionMesh(assetPath))
            {
                if (!importer.addCollider) { importer.addCollider = true; changed = true; }
                if (!importer.isReadable) { importer.isReadable = true; changed = true; }
            }
            return changed;
        }

        private static string Stem(string path)
        {
            return (Path.GetFileNameWithoutExtension(path) ?? string.Empty).ToLowerInvariant();
        }
    }

    /// <summary>Applies <see cref="PolysquishImportSettings"/> to everything under Assets/Polysquish/.</summary>
    internal sealed class PolysquishPostprocessor : AssetPostprocessor
    {
        private void OnPreprocessTexture()
        {
            if (!PolysquishImportSettings.IsPolysquishAsset(assetPath)) return;
            PolysquishImportSettings.ApplyTexture(assetImporter as TextureImporter, assetPath);
        }

        private void OnPreprocessModel()
        {
            if (!PolysquishImportSettings.IsPolysquishAsset(assetPath)) return;
            PolysquishImportSettings.ApplyModel(assetImporter as ModelImporter, assetPath);
        }
    }
}
#endif
