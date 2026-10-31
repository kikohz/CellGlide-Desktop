using CellGlide.Core.Models;

namespace CellGlide.Core.Desktop;

/// <summary>A point in a chart's own coordinates (DIPs, y down).</summary>
public readonly record struct PlotPoint(double X, double Y);

/// <summary>A horizontal span of a strip, in DIPs.</summary>
public readonly record struct StripSpan(double Left, double Width);

/// <summary>
/// The views' 0–1 (or per-mille) chart coordinates as DIPs in a box of the given size: scaling and
/// flipping only; which points there are is the DLL's.
/// </summary>
public static class Geometry
{
    /// <summary>One run of the speed trail: download, or upload with <paramref name="upload"/>.</summary>
    public static PlotPoint[] Plot(IReadOnlyList<TrailPoint> run, bool upload, double width, double height)
    {
        var points = new PlotPoint[run.Count];
        for (var i = 0; i < run.Count; i++)
        {
            points[i] = At(run[i].X, upload ? run[i].Up : run[i].Down, width, height);
        }
        return points;
    }

    /// <summary>A speed test's curve.</summary>
    public static PlotPoint[] Plot(IReadOnlyList<CurvePoint> curve, double width, double height)
    {
        var points = new PlotPoint[curve.Count];
        for (var i = 0; i < curve.Count; i++)
        {
            points[i] = At(curve[i].X, curve[i].Y, width, height);
        }
        return points;
    }

    /// <summary>A cell of the 24-hour strip (its x and width are per mille of the day), at least 1 wide.</summary>
    public static StripSpan Strip(StripCell cell, double width) =>
        new(cell.X / 1000.0 * width, Math.Max(1, cell.Width / 1000.0 * width));

    /// <summary>An hour label's position along the strip (<paramref name="at"/> is 0–1).</summary>
    public static double Tick(double at, double width) => at * width;

    static PlotPoint At(double x, double y, double width, double height) => new(x * width, (1 - y) * height);
}
