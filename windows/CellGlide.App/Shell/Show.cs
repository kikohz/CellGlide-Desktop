using CellGlide.Core.Models;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Media;

namespace CellGlide.App.Shell;

/// <summary>Small display helpers for x:Bind: how a view's value is drawn, never what it says.</summary>
public static class Show
{
    /// <summary>The brush of a tone (Shell/ToneBrushes.xaml, following the app's theme).</summary>
    public static Brush Brush(Tone tone) => Resource("Tone" + tone + "Brush");

    /// <summary>The low-power-mode battery's yellow (Shell/ToneBrushes.xaml).</summary>
    public static Brush LowPowerBrush() => Resource("LowPowerBrush");

    /// <summary>Shown when there is something (a non-empty string counts only when not empty).</summary>
    public static Visibility Visible(object? value) =>
        value is null || value is string { Length: 0 } ? Visibility.Collapsed : Visibility.Visible;

    public static Visibility Visible(bool value) => value ? Visibility.Visible : Visibility.Collapsed;

    /// <summary>Half strength for a dimmed card.</summary>
    public static double Opacity(bool dim) => dim ? 0.5 : 1.0;

    static Brush Resource(string key) => (Brush)Application.Current.Resources[key];
}
