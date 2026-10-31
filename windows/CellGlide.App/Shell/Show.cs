using System.Runtime.InteropServices;
using CellGlide.Core.Models;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Media;
using Windows.UI.ViewManagement;

namespace CellGlide.App.Shell;

/// <summary>Small display helpers for x:Bind: how a view's value is drawn, never what it says.</summary>
public static partial class Show
{
    static readonly UISettings Colors = new();

    /// <summary>
    /// Raised on the UI thread when brushes from <see cref="Brush"/> may have changed (the Windows mode
    /// or high contrast was switched): whoever painted with them paints again.
    /// </summary>
    public static event Action? Changed;

    /// <summary>Starts raising <see cref="Changed"/> on <paramref name="queue"/>'s thread (the app does this once).</summary>
    public static void Watch(DispatcherQueue queue)
    {
        // Raised on a background thread when the Windows mode changes and when high contrast is
        // switched on, off or to another contrast theme. (AccessibilitySettings' own event fails
        // with E_NOINTERFACE in this unpackaged desktop app, so it is not used.)
        Colors.ColorValuesChanged += (_, _) => queue.TryEnqueue(() => Changed?.Invoke());
    }

    /// <summary>
    /// The brush of a tone: Shell/ToneBrushes.xaml's (Light and Dark are the same colours); under high
    /// contrast, the contrast theme's text colour as it is now (grey text for Gray), so a dot never has
    /// a colour the theme does not allow and follows a change of contrast theme.
    /// </summary>
    public static Brush Brush(Tone tone) =>
        HighContrast()
            ? new SolidColorBrush(Colors.UIElementColor(tone == Tone.Gray ? UIElementType.GrayText : UIElementType.WindowText))
            : Resource("Tone" + tone + "Brush");

    /// <summary>The low-power-mode battery's yellow (Shell/ToneBrushes.xaml); the text colour under high contrast.</summary>
    public static Brush LowPowerBrush() =>
        HighContrast()
            ? new SolidColorBrush(Colors.UIElementColor(UIElementType.WindowText))
            : Resource("LowPowerBrush");

    /// <summary>Shown when there is something (a non-empty string counts only when not empty).</summary>
    public static Visibility Visible(object? value) =>
        value is null || value is string { Length: 0 } ? Visibility.Collapsed : Visibility.Visible;

    public static Visibility Visible(bool value) => value ? Visibility.Visible : Visibility.Collapsed;

    /// <summary>Half strength for a dimmed card.</summary>
    public static double Opacity(bool dim) => dim ? 0.5 : 1.0;

    static Brush Resource(string key) => (Brush)Application.Current.Resources[key];

    /// <summary>Whether high contrast is on now (SPI_GETHIGHCONTRAST, HCF_HIGHCONTRASTON).</summary>
    static unsafe bool HighContrast()
    {
        var info = new HIGHCONTRASTW { cbSize = (uint)sizeof(HIGHCONTRASTW) };
        return SystemParametersInfoW(SPI_GETHIGHCONTRAST, info.cbSize, &info, 0) != 0 && (info.dwFlags & HCF_HIGHCONTRASTON) != 0;
    }

    const uint SPI_GETHIGHCONTRAST = 0x0042;
    const uint HCF_HIGHCONTRASTON = 0x00000001;

    [StructLayout(LayoutKind.Sequential)]
    struct HIGHCONTRASTW
    {
        public uint cbSize;
        public uint dwFlags;
        public nint lpszDefaultScheme;
    }

    [LibraryImport("user32.dll")]
    private static unsafe partial int SystemParametersInfoW(uint action, uint param, HIGHCONTRASTW* info, uint winIni);
}
