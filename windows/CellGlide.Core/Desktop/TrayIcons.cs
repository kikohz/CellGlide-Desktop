using CellGlide.Core.Models;

namespace CellGlide.Core.Desktop;

/// <summary>
/// Which of the app's tray icon files shows a <see cref="TrayLook"/> (the look itself is the DLL's).
/// The files are made by scripts/gen-windows-icons.py.
/// </summary>
public static class TrayIcons
{
    /// <summary>
    /// <c>Assets\Tray\cellglide-{idle|active|alert|active-alert}-{light|dark}.ico</c>, relative to the
    /// app's directory; <paramref name="lightTaskbar"/> picks the dark drawing made for a light taskbar.
    /// </summary>
    public static string FileFor(TrayLook look, bool lightTaskbar)
    {
        var state = (look.Active, look.Alert) switch
        {
            (false, false) => "idle",
            (true, false) => "active",
            (false, true) => "alert",
            (true, true) => "active-alert",
        };
        return $@"Assets\Tray\cellglide-{state}-{(lightTaskbar ? "light" : "dark")}.ico";
    }
}
