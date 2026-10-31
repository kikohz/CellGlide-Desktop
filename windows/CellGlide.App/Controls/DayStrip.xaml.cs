using CellGlide.App.Shell;
using CellGlide.Core.Models;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Shapes;
using Windows.Foundation;

namespace CellGlide.App.Controls;

/// <summary>
/// A day's 24-hour strip (cg-gui widgets/strip.rs): a grey ground, then each of the DLL's cells as a
/// rectangle in its tone at its alpha, and the DLL's hour labels under it at their places. Which cells and
/// labels there are, their tones and texts are the DLL's; this only turns per-mille and 0–1 into DIPs.
/// Redrawn when the cells, labels or size change, and when the tone brushes change.
/// </summary>
public sealed partial class DayStrip : UserControl
{
    public static readonly DependencyProperty CellsProperty = DependencyProperty.Register(
        nameof(Cells), typeof(object), typeof(DayStrip), new PropertyMetadata(null, (d, _) => ((DayStrip)d).Redraw()));

    public static readonly DependencyProperty TicksProperty = DependencyProperty.Register(
        nameof(Ticks), typeof(object), typeof(DayStrip), new PropertyMetadata(null, (d, _) => ((DayStrip)d).Redraw()));

    public DayStrip()
    {
        InitializeComponent();
        SizeChanged += (_, _) => Redraw();
        Loaded += (_, _) =>
        {
            Show.Changed -= Redraw;
            Show.Changed += Redraw;
            Redraw();
        };
        Unloaded += (_, _) => Show.Changed -= Redraw;
    }

    /// <summary>The strip's cells (<see cref="HistoryView.Strip"/>), x and width per mille of the day.</summary>
    public IReadOnlyList<StripCell>? Cells
    {
        get => (IReadOnlyList<StripCell>?)GetValue(CellsProperty);
        set => SetValue(CellsProperty, value);
    }

    /// <summary>The hour labels (<see cref="HistoryView.Ticks"/>), each at 0–1 along the strip.</summary>
    public IReadOnlyList<TickView>? Ticks
    {
        get => (IReadOnlyList<TickView>?)GetValue(TicksProperty);
        set => SetValue(TicksProperty, value);
    }

    void Redraw()
    {
        Plot.Children.Clear();
        Labels.Children.Clear();
        var width = Frame.ActualWidth;
        var height = Frame.ActualHeight;
        if (width <= 0 || height <= 0)
        {
            return;
        }
        // The ground: grey at a fifth, as cg-gui paints it before the cells.
        var ground = new Rectangle { Width = width, Height = height, Fill = Show.Brush(Tone.Gray), Opacity = 0.2 };
        Plot.Children.Add(ground);
        foreach (var cell in Cells ?? [])
        {
            var span = Core.Desktop.Geometry.Strip(cell, width);
            var rect = new Rectangle { Width = span.Width, Height = height, Fill = Show.Brush(cell.Tone), Opacity = cell.Alpha };
            Canvas.SetLeft(rect, span.Left);
            Plot.Children.Add(rect);
        }
        foreach (var tick in Ticks ?? [])
        {
            var label = new TextBlock
            {
                Text = tick.Label,
                FontSize = 11,
                Foreground = (Brush)Application.Current.Resources["TextFillColorSecondaryBrush"],
            };
            label.Measure(new Size(double.PositiveInfinity, double.PositiveInfinity));
            var labelWidth = label.DesiredSize.Width;
            // Centred under its hour, kept inside the strip so 00:00 and 24:00 sit at its ends.
            var left = Core.Desktop.Geometry.Tick(tick.At, width) - labelWidth / 2;
            Canvas.SetLeft(label, Math.Clamp(left, 0, Math.Max(0, width - labelWidth)));
            Labels.Children.Add(label);
        }
    }
}
