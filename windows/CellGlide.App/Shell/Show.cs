using System.Runtime.InteropServices;
using CellGlide.App.Hosting;
using CellGlide.Core.Models;
using CellGlide.Core.ViewModels;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation.Peers;
using Microsoft.UI.Xaml.Controls;
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

    /// <summary>
    /// Has a screen reader say <paramref name="line"/> again whenever its text changes to something
    /// (it carries AutomationProperties.LiveSetting; XAML does not raise LiveRegionChanged by itself).
    /// </summary>
    public static void AnnounceChanges(TextBlock line)
    {
        // Every state event sets the text again; only a different text is news.
        var said = line.Text;
        line.RegisterPropertyChangedCallback(TextBlock.TextProperty, (sender, _) =>
        {
            if (sender is not TextBlock changed || changed.Text == said)
            {
                return;
            }
            said = changed.Text;
            if (said.Length > 0
                && (FrameworkElementAutomationPeer.FromElement(changed) ?? FrameworkElementAutomationPeer.CreatePeerForElement(changed)) is { } peer)
            {
                peer.RaiseAutomationEvent(AutomationEvents.LiveRegionChanged);
            }
        });
    }

    // ---- The state as the panel and the overview show it: which of the DLL's texts, never new ones.
    // They take ShellViewModel.Now, which is never null (see there why).
    // Rule for every page: an x:Bind function is only passed paths that are never null (XBindTests checks). ----

    /// <summary>Shown while there is a state.</summary>
    public static Visibility HasState(ShellNow now) => Visible(now.Overview is not null);

    /// <summary>Shown while there is none.</summary>
    public static Visibility NoState(ShellNow now) => Visible(now.Overview is null);

    /// <summary>The state's tone; grey while there is no state (cg_ipc status_tone without a snapshot).</summary>
    public static Brush StatusBrush(ShellNow now) => Brush(now.Overview?.StatusTone ?? Tone.Gray);

    public static string Title(ShellNow now) => now.Overview?.Title ?? "";

    public static string Detail(ShellNow now) => now.Overview?.Detail ?? "";

    /// <summary>Why there is no state (the service-gone sentence), else that the service is being reached.</summary>
    public static string NoStateText(ShellNow now) => now.ServiceDown?.Text ?? L.T("gui.waiting_service");

    /// <summary>The state's title, else why there is none.</summary>
    public static string TitleOrWhy(ShellNow now) => now.Overview?.Title ?? NoStateText(now);

    /// <summary>The last failed command's sentence.</summary>
    public static string ErrorText(ShellNow now) => now.Error ?? "";

    public static Visibility ErrorVisible(ShellNow now) => Visible(now.Error);

    /// <summary>The state's button label; without a state, the tray menu's connect entry.</summary>
    public static string ToggleLabel(ShellNow now) =>
        now.Overview?.ButtonLabel
        ?? now.Chrome?.Menu.FirstOrDefault(item => item.Kind == MenuKind.Toggle)?.Label
        ?? "";

    /// <summary>Prominent for connecting; plain for disconnecting, which is not the thing to invite (cg-gui main_button_style).</summary>
    public static Style ToggleStyle(ShellNow now) =>
        (Style)Application.Current.Resources[now.Overview?.WantsConnection == true ? "DefaultButtonStyle" : "AccentButtonStyle"];

    /// <summary>The DLL's phone card (only when it has one), else nothing.</summary>
    public static PhoneCardView? Phone(ShellNow now) => now.Overview?.Phone;

    public static Visibility PhoneVisible(ShellNow now) => Visible(now.Overview?.Phone);

    /// <summary>The health row, only while the DLL gives one (connected).</summary>
    public static Visibility HealthVisible(ShellNow now) => Visible(now.Overview?.Health);

    /// <summary>The health row's dot.</summary>
    public static Brush HealthBrush(ShellNow now) => Brush(now.Overview?.Health?.Tone ?? Tone.Gray);

    /// <summary>The health row's line (the DLL's), or nothing.</summary>
    public static string HealthLine(ShellNow now) => now.Overview?.Health?.Line ?? "";

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
