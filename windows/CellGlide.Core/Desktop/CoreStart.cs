using CellGlide.Core.Dispatch;

namespace CellGlide.Core.Desktop;

/// <summary>Starting the DLL, telling a broken installation apart from a bug.</summary>
public static class CoreStart
{
    /// <summary>
    /// Runs <paramref name="start"/>: the core, or null when the DLL cannot be loaded (missing, the
    /// wrong architecture, an old copy without the entry point) or cg_init refuses. Anything else is
    /// a bug and is passed on.
    /// </summary>
    public static ICore? TryStart(Func<ICore> start)
    {
        try
        {
            return start();
        }
        catch (Exception e) when (e is DllNotFoundException or EntryPointNotFoundException or BadImageFormatException or InvalidOperationException)
        {
            return null;
        }
    }
}
