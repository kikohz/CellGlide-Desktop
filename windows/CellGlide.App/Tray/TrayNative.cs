using System.Runtime.InteropServices;
using CellGlide.Core.Desktop;
using Microsoft.Win32;

namespace CellGlide.App.Tray;

/// <summary>The few shell and monitor calls the tray and its panel need; every result in physical pixels.</summary>
static unsafe partial class TrayNative
{
    /// <summary>Where the tray icon with this GUID is now (in the taskbar or its overflow), or null.</summary>
    public static PixelRect? IconRect(Guid id)
    {
        var identifier = new NOTIFYICONIDENTIFIER
        {
            cbSize = (uint)sizeof(NOTIFYICONIDENTIFIER),
            guidItem = id,
        };
        RECT rect;
        return Shell_NotifyIconGetRect(&identifier, &rect) == 0 ? rect.ToPixels() : null;
    }

    /// <summary>The side the (primary) taskbar is on; bottom if the shell does not say.</summary>
    public static TaskbarEdge TaskbarEdge()
    {
        var data = new APPBARDATA { cbSize = (uint)sizeof(APPBARDATA) };
        if (SHAppBarMessage(ABM_GETTASKBARPOS, &data) == 0)
        {
            return Core.Desktop.TaskbarEdge.Bottom;
        }
        return data.uEdge switch
        {
            ABE_LEFT => Core.Desktop.TaskbarEdge.Left,
            ABE_TOP => Core.Desktop.TaskbarEdge.Top,
            ABE_RIGHT => Core.Desktop.TaskbarEdge.Right,
            _ => Core.Desktop.TaskbarEdge.Bottom,
        };
    }

    /// <summary>
    /// The work area and effective DPI of the monitor the icon is on; without an icon, of the monitor
    /// under the mouse pointer.
    /// </summary>
    public static (PixelRect WorkArea, uint Dpi) WorkArea(PixelRect? icon)
    {
        nint monitor;
        if (icon is { } r)
        {
            var rect = new RECT { left = r.Left, top = r.Top, right = r.Right, bottom = r.Bottom };
            monitor = MonitorFromRect(&rect, MONITOR_DEFAULTTONEAREST);
        }
        else
        {
            POINT point;
            if (GetCursorPos(&point) == 0)
            {
                point = default;
            }
            monitor = MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST);
        }
        var info = new MONITORINFO { cbSize = (uint)sizeof(MONITORINFO) };
        _ = GetMonitorInfoW(monitor, &info);
        uint dpiX, dpiY;
        if (GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &dpiX, &dpiY) != 0 || dpiX == 0)
        {
            dpiX = 96;
        }
        return (info.rcWork.ToPixels(), dpiX);
    }

    /// <summary>Physical pixels of <paramref name="dips"/> at <paramref name="dpi"/>.</summary>
    public static int Scale(int dips, uint dpi) => (int)Math.Round(dips * dpi / 96.0);

    /// <summary>
    /// The size of a small icon (the tray's), in pixels, at the current DPI of the monitor the icon is
    /// on; before the shell has placed the icon, of the primary monitor (where the notification area
    /// is). Read afresh each time, so a changed scale is picked up.
    /// </summary>
    public static int TrayIconSize(PixelRect? icon)
    {
        nint monitor;
        if (icon is { } r)
        {
            var rect = new RECT { left = r.Left, top = r.Top, right = r.Right, bottom = r.Bottom };
            monitor = MonitorFromRect(&rect, MONITOR_DEFAULTTONEAREST);
        }
        else
        {
            monitor = MonitorFromPoint(default, MONITOR_DEFAULTTOPRIMARY);
        }
        uint dpiX, dpiY;
        if (GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &dpiX, &dpiY) != 0 || dpiX == 0)
        {
            dpiX = 96;
        }
        return GetSystemMetricsForDpi(SM_CXSMICON, dpiX) is > 0 and var size ? size : 16;
    }

    /// <summary>Whether the taskbar is light (Settings, Personalization, Colors: Windows mode light).</summary>
    public static bool LightTaskbar()
    {
        try
        {
            using var key = Registry.CurrentUser.OpenSubKey(@"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
            return key?.GetValue("SystemUsesLightTheme") is int value && value != 0;
        }
        catch (Exception)
        {
            return false;
        }
    }

    const uint ABM_GETTASKBARPOS = 5;
    const uint ABE_LEFT = 0;
    const uint ABE_TOP = 1;
    const uint ABE_RIGHT = 2;
    const uint MONITOR_DEFAULTTOPRIMARY = 1;
    const uint MONITOR_DEFAULTTONEAREST = 2;
    const int MDT_EFFECTIVE_DPI = 0;
    const int SM_CXSMICON = 49;

    [StructLayout(LayoutKind.Sequential)]
    struct RECT
    {
        public int left, top, right, bottom;

        public readonly PixelRect ToPixels() => new(left, top, right, bottom);
    }

    [StructLayout(LayoutKind.Sequential)]
    struct POINT
    {
        public int x, y;
    }

    [StructLayout(LayoutKind.Sequential)]
    struct NOTIFYICONIDENTIFIER
    {
        public uint cbSize;
        public nint hWnd;
        public uint uID;
        public Guid guidItem;
    }

    [StructLayout(LayoutKind.Sequential)]
    struct APPBARDATA
    {
        public uint cbSize;
        public nint hWnd;
        public uint uCallbackMessage;
        public uint uEdge;
        public RECT rc;
        public nint lParam;
    }

    [StructLayout(LayoutKind.Sequential)]
    struct MONITORINFO
    {
        public uint cbSize;
        public RECT rcMonitor;
        public RECT rcWork;
        public uint dwFlags;
    }

    [LibraryImport("shell32.dll")]
    private static partial int Shell_NotifyIconGetRect(NOTIFYICONIDENTIFIER* identifier, RECT* iconLocation);

    [LibraryImport("shell32.dll")]
    private static partial nuint SHAppBarMessage(uint message, APPBARDATA* data);

    [LibraryImport("user32.dll")]
    private static partial nint MonitorFromRect(RECT* rect, uint flags);

    [LibraryImport("user32.dll")]
    private static partial nint MonitorFromPoint(POINT point, uint flags);

    [LibraryImport("user32.dll")]
    private static partial int GetCursorPos(POINT* point);

    [LibraryImport("user32.dll")]
    private static partial int GetMonitorInfoW(nint monitor, MONITORINFO* info);

    [LibraryImport("shcore.dll")]
    private static partial int GetDpiForMonitor(nint monitor, int dpiType, uint* dpiX, uint* dpiY);

    [LibraryImport("user32.dll")]
    private static partial int GetSystemMetricsForDpi(int index, uint dpi);
}
