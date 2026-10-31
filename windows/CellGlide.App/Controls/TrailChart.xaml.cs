using CellGlide.App.Shell;
using CellGlide.Core.Models;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Shapes;
using Windows.Foundation;

namespace CellGlide.App.Controls;

/// <summary>
/// The last minute of download (blue) and upload (purple), as cg-gui's widgets/trail.rs: one pair of
/// lines per run the DLL gives (a second without a reading is a break, never a drop to zero), and a
/// dot for a run of one reading. Redrawn when the runs or the size change.
/// </summary>
public sealed partial class TrailChart : UserControl
{
    /// <summary>Room kept above and below the lines, so the top and bottom points are not clipped.</summary>
    const double Inset = 1;

    public static readonly DependencyProperty RunsProperty = DependencyProperty.Register(
        nameof(Runs), typeof(object), typeof(TrailChart), new PropertyMetadata(null, (d, _) => ((TrailChart)d).Redraw()));

    public TrailChart()
    {
        InitializeComponent();
        SizeChanged += (_, _) => Redraw();
    }

    /// <summary>The trail's runs (<see cref="PhoneCardView.Trail"/>), x and values 0–1.</summary>
    public IReadOnlyList<IReadOnlyList<TrailPoint>>? Runs
    {
        get => (IReadOnlyList<IReadOnlyList<TrailPoint>>?)GetValue(RunsProperty);
        set => SetValue(RunsProperty, value);
    }

    void Redraw()
    {
        Plot.Children.Clear();
        var width = ActualWidth;
        var height = ActualHeight - 2 * Inset;
        if (Runs is not { } runs || width <= 0 || height <= 0)
        {
            return;
        }
        foreach (var run in runs)
        {
            foreach (var upload in new[] { false, true })
            {
                var brush = Show.Brush(upload ? Tone.Purple : Tone.Blue);
                var points = Core.Desktop.Geometry.Plot(run, upload, width, height);
                if (points.Length == 1)
                {
                    // One reading between two gaps: a dot 2 across.
                    var dot = new Ellipse { Width = 2, Height = 2, Fill = brush };
                    Canvas.SetLeft(dot, points[0].X - 1);
                    Canvas.SetTop(dot, points[0].Y + Inset - 1);
                    Plot.Children.Add(dot);
                }
                else if (points.Length > 1)
                {
                    var line = new Polyline
                    {
                        Stroke = brush,
                        StrokeThickness = 1.5,
                        StrokeLineJoin = PenLineJoin.Round,
                    };
                    foreach (var p in points)
                    {
                        line.Points.Add(new Point(p.X, p.Y + Inset));
                    }
                    Plot.Children.Add(line);
                }
            }
        }
    }
}
