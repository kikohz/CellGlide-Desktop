using CellGlide.App.Shell;
using CellGlide.Core.Models;
using CellGlide.Core.ViewModels;
using CommunityToolkit.Mvvm.Input;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;

namespace CellGlide.App.Pages;

/// <summary>
/// The overview (cg-gui pages/overview.rs) bound to <see cref="ShellViewModel"/>: with a state, its
/// dot, title, sentence and strict DNS line, the phone card and the health row; without one, only
/// why (or that the service is being reached). The button is the state's, off while its command
/// runs (through Show.SetEnabled).
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
            // Loaded can come twice without an Unloaded between: never two handlers.
            Show.Changed -= Repaint;
            Show.Changed += Repaint;
            Repaint();
            ApplyBusy();
        };
        // The button is off while its command runs (from here, the panel or the tray menu).
        shell.ToggleCommand.PropertyChanged += (_, e) =>
        {
            if (e.PropertyName == nameof(IAsyncRelayCommand.IsRunning))
            {
                ApplyBusy();
            }
        };
        Unloaded += (_, _) => Show.Changed -= Repaint;
        // A new state, a reason the service is away or a failed command is read out.
        Show.AnnounceChanges(StateTitle);
        Show.AnnounceChanges(NoStateLine);
        Show.AnnounceChanges(ErrorLine);
    }

    public ShellViewModel Shell { get; }

    void Repaint() => Bindings.Update();

    /// <summary>
    /// Off while the toggle runs, through Show.SetEnabled: keyboard focus goes to Change... and comes back when
    /// it is done (a Command binding would turn the button off with the focus still on it).
    /// </summary>
    void ApplyBusy() => Show.SetEnabled(ToggleButton, !Shell.ToggleCommand.IsRunning, ChangeButton);

    async void OnToggleClick(object sender, RoutedEventArgs e)
    {
        // A second press while it runs is refused here as the command would refuse it (cg-gui's command_pending).
        if (Shell.ToggleCommand.CanExecute(null))
        {
            // A failed toggle leaves its sentence in Error, shown above.
            await Shell.ToggleCommand.ExecuteAsync(null);
        }
    }

    void OnHealthClick(object sender, RoutedEventArgs e) => _navigate(Section.History);

    void OnChangeClick(object sender, RoutedEventArgs e) => _navigate(Section.Settings);

    void OnAppleDevicesClick(object sender, RoutedEventArgs e) => _ = App.Current.AppleDevices.OpenAsync();

    // ---- For x:Bind (the rest is in Show): which of the DLL's texts and tones to show, never new ones. ----

    Brush ErrorBrush => Show.Brush(Tone.Red);

    string StrictDnsOf(ShellNow now) => now.Overview?.StrictDns ?? "";

    Visibility StrictDnsVisible(ShellNow now) => Show.Visible(now.Overview?.StrictDns);

    string DeviceOf(ShellNow now) => now.Overview?.DeviceSubtitle ?? "";

    Visibility DeviceVisible(ShellNow now) => Show.Visible(now.Overview?.DeviceSubtitle);
}
