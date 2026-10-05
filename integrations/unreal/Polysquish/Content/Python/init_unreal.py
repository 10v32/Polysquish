"""Executed automatically by the Python Editor Script Plugin when the Polysquish plugin is enabled.

Registers the Tools ▸ Polysquish menu and the toolbar button.
"""

try:
    import unreal  # noqa: F401  (only available inside the Unreal Editor)
    import polysquish_tool

    polysquish_tool.register()
except Exception as exc:  # pragma: no cover - never break editor start-up
    try:
        import unreal

        unreal.log_error("[Polysquish] failed to register the editor menu: {}".format(exc))
    except Exception:
        print("[Polysquish] failed to register the editor menu: {}".format(exc))
