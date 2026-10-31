namespace CellGlide.Core.Desktop;

/// <summary>Which side of its monitor the taskbar is on.</summary>
public enum TaskbarEdge
{
    Bottom,
    Top,
    Left,
    Right,
}

/// <summary>A rectangle in physical screen pixels (right and bottom exclusive, as Win32's RECT).</summary>
public readonly record struct PixelRect(int Left, int Top, int Right, int Bottom);

/// <summary>A point in physical screen pixels.</summary>
public readonly record struct PixelPoint(int X, int Y);

/// <summary>Where the tray panel opens: next to its icon, on the taskbar's side, inside the work area.</summary>
public static class PanelPlacement
{
    /// <summary>
    /// The panel's top-left corner, all in physical pixels. Against the taskbar's edge (a margin away
    /// from the work area's border), centred on the icon along it; without an icon, at the end of that
    /// edge (bottom right; bottom left for a left taskbar). Then kept a margin inside the work area,
    /// or at its left or top when the panel does not fit.
    /// </summary>
    public static PixelPoint Place(PixelRect? icon, PixelRect workArea, TaskbarEdge edge, int width, int height, int margin)
    {
        var right = workArea.Right - margin - width;
        var bottom = workArea.Bottom - margin - height;
        int x, y;
        switch (edge)
        {
            case TaskbarEdge.Top:
                y = workArea.Top + margin;
                x = icon is { } t ? Middle(t.Left, t.Right) - width / 2 : right;
                break;
            case TaskbarEdge.Left:
                x = workArea.Left + margin;
                y = icon is { } l ? Middle(l.Top, l.Bottom) - height / 2 : bottom;
                break;
            case TaskbarEdge.Right:
                x = right;
                y = icon is { } r ? Middle(r.Top, r.Bottom) - height / 2 : bottom;
                break;
            default:
                y = bottom;
                x = icon is { } b ? Middle(b.Left, b.Right) - width / 2 : right;
                break;
        }
        return new PixelPoint(
            Clamp(x, workArea.Left + margin, right, workArea.Left),
            Clamp(y, workArea.Top + margin, bottom, workArea.Top));
    }

    /// <summary>The middle, rounded down (also for negative coordinates).</summary>
    static int Middle(int a, int b) => (a + b) >> 1;

    /// <summary><paramref name="value"/> within [low, high]; <paramref name="fallback"/> when that range is empty.</summary>
    static int Clamp(int value, int low, int high, int fallback) =>
        low > high ? fallback : Math.Clamp(value, low, high);
}
