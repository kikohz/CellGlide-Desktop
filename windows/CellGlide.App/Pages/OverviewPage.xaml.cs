using CellGlide.App.Shell;
using CellGlide.Core.Models;
using CellGlide.Core.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;

namespace CellGlide.App.Pages;

/// <summary>
/// The overview (cg-gui pages/overview.rs) bound to <see cref="ShellViewModel"/>: with a state, its
/// dot, title, sentence and strict DNS line, the phone card and the health row; without one, only
/// why (or that the service is being reached). The button is the state's, disabled while its
/// command runs.
/// </summary>
public sealed partial class OverviewPage : Page
{
    readonly Action<Section> _navigate;

    /// <param name="navigate">Opens another page of the main window.</param>
    public OverviewPage(ShellViewModel shell, Action<Section> navigate)
    {
        Shell = shell;
        _navigate = navigate;
        InitializeComponent();
        // The dots' brushes follow a switch to or from high contrast while the page is shown, and
        // are made again when it comes back (the switch may have happened while it was away).
        Loaded += (_, _) =>
        {
            Show.Changed += Repaint;
            Repaint();
        };
        Unloaded += (_, _) => Show.Changed -= Repaint;
        // A new state, a reason the service is away or a failed command is read out.
        Show.AnnounceChanges(StateTitle);
        Show.AnnounceChanges(NoStateLine);
        Show.AnnounceChanges(ErrorLine);
    }

    public ShellViewModel Shell { get; }

    void Repaint() => Bindings.Update();

    void OnHealthClick(object sender, RoutedEventArgs e) => _navigate(Section.History);

    void OnChangeClick(object sender, RoutedEventArgs e) => _navigate(Section.Settings);

    // ---- For x:Bind (the rest is in Show): which of the DLL's texts and tones to show, never new ones. ----

    Brush ErrorBrush => Show.Brush(Tone.Red);

    string StrictDnsOf(ShellNow now) => now.Overview?.StrictDns ?? "";

    Visibility StrictDnsVisible(ShellNow now) => Show.Visible(now.Overview?.StrictDns);

    string DeviceOf(ShellNow now) => now.Overview?.DeviceSubtitle ?? "";

    Visibility DeviceVisible(ShellNow now) => Show.Visible(now.Overview?.DeviceSubtitle);
}
