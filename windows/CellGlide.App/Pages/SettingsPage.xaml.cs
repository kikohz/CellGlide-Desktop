using System.ComponentModel;
using CellGlide.App.Hosting;
using CellGlide.App.Shell;
using CellGlide.Core.Desktop;
using CellGlide.Core.Models;
using CellGlide.Core.ViewModels;
using Microsoft.UI.Text;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;

namespace CellGlide.App.Pages;

/// <summary>
/// Settings (cg-gui pages/settings.rs). The service's settings (iPhone, connect automatically, strict DNS) are
/// shown from the state's <see cref="SettingsView"/> and changed with one SetSettings each: a switch shows the
/// service's value, only a user's change to something else is sent (<see cref="ServiceToggle"/>), and it is
/// off while that change is on its way. Without a state those controls are off; start at login and the
/// preferences work as always. Start at login is our own Run value (<see cref="Autostart"/>, never
/// StartupApproved): read each time the page shows or the window comes forward, written on a click, then
/// read back (<see cref="AutostartToggle"/>). Filled in code (x:Bind functions stop at a null path).
/// <para>Wireless connection: its switch is sent the same way, but turning it on is checked by the service first
/// (up to half a minute): meanwhile the switch shows what was asked, is off, and "checking" shows; a refusal
/// says why under it. With the switch on: the paired iPhones (Pair Again, Remove after a confirmation) and New
/// Network Name and Password (after a confirmation); Pair Again and New Network Name are off while an attempt
/// runs, and Pair Again goes on to the overview, where the attempt shows.</para>
/// </summary>
public sealed partial class SettingsPage : Page
{
    readonly SettingsViewModel _settings;
    readonly WirelessViewModel _wireless;
    readonly ShellViewModel _shell;
    readonly Action<Section> _navigate;
    /// <summary>What the user turned the wireless switch to, while that change is on its way (it can take half a minute).</summary>
    bool? _wirelessWanted;
    /// <summary>The paired iPhones the rows show, and each row's buttons.</summary>
    IReadOnlyList<PairedPhoneView> _pairedShown = [];
    readonly List<(Button Repair, Button Forget)> _pairedButtons = [];
    readonly Autostart _autostart;
    /// <summary>Set while the code itself moves the controls, so their events are not taken for a user's.</summary>
    bool _rendering;
    AutostartState _autostartState;
    // Why reading the Run key failed (cleared by the next read that works), and why the last click's write failed.
    string? _autostartReadError;
    string? _autostartWriteError;
    readonly CopyFeedback _copy;

    /// <param name="navigate">Opens another page of the main window (the overview, once a re-pairing has started).</param>
    public SettingsPage(SettingsViewModel settings, WirelessViewModel wireless, ShellViewModel shell, Autostart autostart, Window window, Action<Section> navigate)
    {
        _settings = settings;
        _wireless = wireless;
        _shell = shell;
        _navigate = navigate;
        _autostart = autostart;
        _copy = new CopyFeedback(Render);
        InitializeComponent();
        Checks.Checks = settings.Checks;
        Checks.RunLabel = L.T("settings.run_checks");

        settings.PropertyChanged += OnChanged;
        shell.PropertyChanged += OnChanged;
        wireless.PropertyChanged += OnChanged;
        foreach (var command in new INotifyPropertyChanged[]
                 {
                     settings.SetAutoConnectCommand, settings.SetPhoneCommand, settings.SetStrictDnsCommand, settings.SetPrefsCommand,
                     wireless.SetEnabledCommand, wireless.ForgetCommand, wireless.RepairCommand, wireless.RegenerateCommand,
                 })
        {
            command.PropertyChanged += (_, e) =>
            {
                if (e.PropertyName == "IsRunning")
                {
                    Render();
                }
            };
        }
        // Coming back from Windows' Startup apps: what it says may have changed.
        window.Activated += (_, e) =>
        {
            if (e.WindowActivationState != WindowActivationState.Deactivated && IsLoaded)
            {
                ReadAutostart();
            }
        };
        Loaded += (_, _) =>
        {
            Show.Changed -= Render;
            Show.Changed += Render;
            ReadAutostart();
        };
        Unloaded += (_, _) => Show.Changed -= Render;
        Show.AnnounceChanges(ErrorLine);
        Show.AnnounceChanges(AutostartErrorLine);
        Show.AnnounceChanges(DnsText);
        Show.AnnounceChanges(WirelessUnavailableLine);
        Show.AnnounceChanges(WirelessErrorLine);
        ReadAutostart();
    }

    SettingsView? Service => _shell.Overview?.Settings;

    void OnChanged(object? sender, PropertyChangedEventArgs e)
    {
        // Error is the settings' or the wireless one's (both are shown).
        if (e.PropertyName is nameof(ShellViewModel.Now) or nameof(SettingsViewModel.Prefs) or nameof(CoreViewModel.Error)
            or nameof(WirelessViewModel.SwitchBusy))
        {
            Render();
        }
    }

    void ReadAutostart()
    {
        try
        {
            _autostartState = _autostart.Read();
            _autostartReadError = null;
        }
        catch (Exception e) when (e is UnauthorizedAccessException or System.Security.SecurityException or IOException)
        {
            _autostartState = AutostartState.Off;
            _autostartReadError = L.T("settings.autostart_failed", "error", e.Message);
        }
        Render();
    }

    void Render()
    {
        _rendering = true;
        try
        {
            var service = Service;

            ErrorLine.Text = _settings.Error ?? "";
            ErrorLine.Foreground = Show.Brush(Tone.Red);
            ErrorLine.Visibility = Show.Visible(_settings.Error);

            // Which iPhone: the DLL's labels, its selection.
            var phones = service?.Phones ?? [];
            var labels = phones.Select(p => p.Label).ToList();
            if (!PhoneChooser.Items.Cast<object>().Select(o => o as string).SequenceEqual(labels))
            {
                PhoneChooser.Items.Clear();
                foreach (var label in labels)
                {
                    PhoneChooser.Items.Add(label);
                }
            }
            var selected = service is not null && service.SelectedPhone >= 0 && service.SelectedPhone < phones.Count ? service.SelectedPhone : -1;
            if (PhoneChooser.SelectedIndex != selected)
            {
                PhoneChooser.SelectedIndex = selected;
            }

            // The service's switches show its values.
            AutoConnectSwitch.IsOn = service?.AutoConnect ?? false;
            StrictDnsSwitch.IsOn = service?.StrictDns ?? false;
            var wireless = RenderWireless(service?.Wireless);

            // Start at login: what the Run key says; off and fixed while Windows has turned us off.
            var disabled = _autostartState == AutostartState.DisabledBySystem;
            AutostartSwitch.IsOn = _autostartState == AutostartState.On;
            AutomationProperties.SetHelpText(AutostartSwitch, L.T(disabled ? "settings.launch_disabled_by_system" : "settings.launch_note"));
            DisabledBySystemLine.Foreground = Show.Brush(Tone.Orange);
            // Its text never changes, so XAML says nothing by itself: said when it appears.
            var appears = disabled && DisabledBySystemLine.Visibility != Visibility.Visible;
            DisabledBySystemLine.Visibility = Show.Visible(disabled);
            if (appears)
            {
                Show.Announce(DisabledBySystemLine);
            }
            // Never together with the disabled line: while Windows has turned us off, that line is the one that counts.
            var autostartError = disabled ? null : _autostartWriteError ?? _autostartReadError;
            AutostartErrorLine.Text = autostartError ?? "";
            AutostartErrorLine.Foreground = Show.Brush(Tone.Red);
            AutostartErrorLine.Visibility = Show.Visible(autostartError);

            // Open on plug: the app's preference.
            OpenOnPlugSwitch.IsOn = _settings.Prefs?.OpenOnPlug ?? false;

            // The proxy address, or that there is none yet.
            var proxy = service?.Proxy;
            ProxyText.Text = proxy ?? L.T("settings.proxy_none");
            ProxyText.FontFamily = proxy is null ? FontFamily : new Microsoft.UI.Xaml.Media.FontFamily("Consolas");
            ProxyText.Style = Show.ThemeStyle(proxy is null ? "SecondaryText" : "PrimaryText");
            CopyFeedback.SetLabel(CopyProxyButton, _copy.Copied is not null);
            // Off while their change is on its way (or without a state); all at once, so a control going off hands focus
            // to its nearest neighbour that is on, and gets it back when the change is done.
            // The wireless switch is off while its change is on its way or being checked; Pair Again and New Network
            // Name while an attempt runs (the service would refuse them).
            var busy = wireless?.Busy == true;
            var paired = _pairedButtons
                .SelectMany(b => new (Control, bool)[]
                {
                    (b.Repair, !busy && !_wireless.RepairCommand.IsRunning),
                    (b.Forget, !_wireless.ForgetCommand.IsRunning),
                });
            Show.SetEnabled(
                [
                    (PhoneChooser, service is not null && !_settings.SetPhoneCommand.IsRunning),
                    (AutoConnectSwitch, service is not null && !_settings.SetAutoConnectCommand.IsRunning),
                    (WirelessSwitch, service is not null && !_wireless.SwitchBusy),
                    .. paired,
                    (RegenerateButton, !busy && !_wireless.RegenerateCommand.IsRunning),
                    (AutostartSwitch, !disabled),
                    (OpenOnPlugSwitch, _settings.Prefs is not null && !_settings.SetPrefsCommand.IsRunning),
                    (StrictDnsSwitch, service is not null && !_settings.SetStrictDnsCommand.IsRunning),
                    (CopyProxyButton, proxy is not null),
                ],
                [
                    PhoneChooser, AutoConnectSwitch, WirelessSwitch,
                    .. _pairedButtons.SelectMany(b => new Control[] { b.Repair, b.Forget }),
                    RegenerateButton, AutostartSwitch, OpenOnPlugSwitch, StrictDnsSwitch, CopyProxyButton, Checks.RunControl,
                ]);

            // DNS: the DLL's line and tone.
            DnsText.Text = service?.Dns.Text ?? "";
            DnsDot.Fill = Show.Brush(service?.Dns.Tone ?? Tone.Gray);
            DnsRow.Visibility = Show.Visible(service is not null);
        }
        finally
        {
            _rendering = false;
        }
    }

    /// <summary>
    /// The wireless card: the switch (the value the user asked for while it is on its way, else the service's),
    /// "checking" while turning it on is checked, why it can't work here, a refused command, the paired iPhones
    /// (switch on and some paired) and New Network Name (switch on). Returns the service's wireless settings.
    /// </summary>
    WirelessSettingsView? RenderWireless(WirelessSettingsView? wireless)
    {
        var enabled = wireless?.Enabled ?? false;
        WirelessSwitch.IsOn = _wirelessWanted ?? enabled;
        var checking = wireless?.Checking == true || (_wirelessWanted == true && _wireless.SetEnabledCommand.IsRunning);
        // Its text never changes, so XAML says nothing by itself: said when it appears.
        var appears = checking && WirelessCheckingRow.Visibility != Visibility.Visible;
        WirelessCheckingRow.Visibility = Show.Visible(checking);
        WirelessCheckingRing.IsActive = checking;
        if (appears)
        {
            Show.Announce(WirelessCheckingText);
        }
        WirelessCheckingText.Style = Show.ThemeStyle("SecondaryText");
        var unavailable = wireless?.AvailableText;
        WirelessUnavailableLine.Text = unavailable ?? "";
        WirelessUnavailableLine.Foreground = Show.Brush(Tone.Orange);
        WirelessUnavailableLine.Visibility = Show.Visible(unavailable);
        WirelessErrorLine.Text = _wireless.Error ?? "";
        WirelessErrorLine.Foreground = Show.Brush(Tone.Red);
        WirelessErrorLine.Visibility = Show.Visible(_wireless.Error);

        var phones = enabled ? wireless!.Paired : [];
        if (!phones.SequenceEqual(_pairedShown))
        {
            BuildPairedRows(phones);
        }
        PairedSection.Visibility = Show.Visible(phones.Count > 0);
        RegenerateButton.Visibility = Show.Visible(enabled);
        return wireless;
    }

    /// <summary>One row per paired iPhone: its name, how it was paired, "Pair again" when it went stale, and its buttons.</summary>
    void BuildPairedRows(IReadOnlyList<PairedPhoneView> phones)
    {
        PairedList.Children.Clear();
        _pairedButtons.Clear();
        foreach (var phone in phones)
        {
            var row = new Grid { ColumnSpacing = 8 };
            row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
            row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
            row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
            var text = new StackPanel { Spacing = 2, VerticalAlignment = VerticalAlignment.Center };
            text.Children.Add(new TextBlock { Text = phone.Name, FontSize = 13, TextWrapping = TextWrapping.Wrap });
            text.Children.Add(new TextBlock { Text = phone.Method, FontSize = 11, TextWrapping = TextWrapping.Wrap, Style = Show.ThemeStyle("SecondaryText") });
            if (phone.Stale)
            {
                text.Children.Add(new TextBlock { Text = L.T("wireless.paired.stale"), FontSize = 11, TextWrapping = TextWrapping.Wrap, Foreground = Show.Brush(Tone.Orange) });
            }
            // A stale pairing must be made again; a good one may be (it is replaced once the new one is saved).
            var repair = new Button
            {
                Content = L.T("wireless.action.repair"),
                FontSize = 12,
                Padding = new Thickness(8, 2, 8, 3),
                VerticalAlignment = VerticalAlignment.Center,
                Tag = phone.PeerId,
            };
            if (phone.Stale)
            {
                repair.Style = (Style)Application.Current.Resources["AccentButtonStyle"];
            }
            var forget = new Button
            {
                Content = L.T("wireless.action.forget"),
                FontSize = 12,
                Padding = new Thickness(8, 2, 8, 3),
                VerticalAlignment = VerticalAlignment.Center,
                Tag = phone,
            };
            AutomationProperties.SetHelpText(repair, phone.Name);
            AutomationProperties.SetHelpText(forget, phone.Name);
            repair.Click += OnRepairClick;
            forget.Click += OnForgetClick;
            Grid.SetColumn(repair, 1);
            Grid.SetColumn(forget, 2);
            row.Children.Add(text);
            row.Children.Add(repair);
            row.Children.Add(forget);
            PairedList.Children.Add(row);
            _pairedButtons.Add((repair, forget));
        }
        _pairedShown = phones;
    }

    async void OnWirelessToggled(object sender, RoutedEventArgs e)
    {
        var on = WirelessSwitch.IsOn;
        if (_rendering || !ServiceToggle.ShouldSend(on, Service?.Wireless.Enabled, _wireless.SwitchBusy))
        {
            return;
        }
        // The switch keeps showing what was asked for while the service checks (up to half a minute); a refusal
        // leaves its reason under it and the switch shows the service's value again.
        _wirelessWanted = on;
        Render();
        await _wireless.SetEnabledCommand.ExecuteAsync(on);
        _wirelessWanted = null;
        Render();
    }

    async void OnRepairClick(object sender, RoutedEventArgs e)
    {
        if (sender is not Button { Tag: string peer } || _wireless.RepairCommand.IsRunning)
        {
            return;
        }
        await _wireless.RepairCommand.ExecuteAsync(peer);
        if (_wireless.Error is null)
        {
            // The attempt runs on the overview: its steps, and the code dialog there.
            _navigate(Section.Overview);
        }
    }

    async void OnForgetClick(object sender, RoutedEventArgs e)
    {
        if (sender is not Button { Tag: PairedPhoneView phone } || _wireless.ForgetCommand.IsRunning)
        {
            return;
        }
        if (await ConfirmAsync(L.T("wireless.action.forget_confirm", "name", phone.Name), L.T("wireless.action.forget")))
        {
            await _wireless.ForgetCommand.ExecuteAsync(phone.PeerId);
        }
    }

    async void OnRegenerateClick(object sender, RoutedEventArgs e)
    {
        if (_wireless.RegenerateCommand.IsRunning)
        {
            return;
        }
        if (await ConfirmAsync(L.T("wireless.action.regenerate_confirm"), L.T("common.continue")))
        {
            await _wireless.RegenerateCommand.ExecuteAsync(null);
        }
    }

    /// <summary>Asks before something that cannot be taken back; Cancel is the default. A dialog that cannot be shown (another is up) is a no.</summary>
    async Task<bool> ConfirmAsync(string question, string yes)
    {
        var dialog = new ContentDialog
        {
            XamlRoot = XamlRoot,
            Content = new TextBlock { Text = question, FontSize = 14, FontWeight = FontWeights.SemiBold, TextWrapping = TextWrapping.Wrap },
            PrimaryButtonText = yes,
            CloseButtonText = L.T("common.cancel"),
            DefaultButton = ContentDialogButton.Close,
        };
        AutomationProperties.SetName(dialog, question);
        try
        {
            return await dialog.ShowAsync() == ContentDialogResult.Primary;
        }
        catch (Exception)
        {
            return false;
        }
    }

    async void OnPhoneChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_rendering || Service is not { } service || _settings.SetPhoneCommand.IsRunning)
        {
            return;
        }
        var index = PhoneChooser.SelectedIndex;
        if (index < 0 || index >= service.Phones.Count || index == service.SelectedPhone)
        {
            return;
        }
        // A refusal leaves its sentence in Error; the chooser then shows the service's choice again.
        await _settings.SetPhoneCommand.ExecuteAsync(service.Phones[index].Choice);
        Render();
    }

    async void OnAutoConnectToggled(object sender, RoutedEventArgs e)
    {
        var on = AutoConnectSwitch.IsOn;
        if (_rendering || !ServiceToggle.ShouldSend(on, Service?.AutoConnect, _settings.SetAutoConnectCommand.IsRunning))
        {
            return;
        }
        await _settings.SetAutoConnectCommand.ExecuteAsync(on);
        Render();
    }

    async void OnStrictDnsToggled(object sender, RoutedEventArgs e)
    {
        var on = StrictDnsSwitch.IsOn;
        if (_rendering || !ServiceToggle.ShouldSend(on, Service?.StrictDns, _settings.SetStrictDnsCommand.IsRunning))
        {
            return;
        }
        await _settings.SetStrictDnsCommand.ExecuteAsync(on);
        Render();
    }

    void OnAutostartToggled(object sender, RoutedEventArgs e)
    {
        var on = AutostartSwitch.IsOn;
        if (_rendering || on == (_autostartState == AutostartState.On))
        {
            return;
        }
        // The switch follows the Run key, not the click: written, then read back.
        try
        {
            var change = AutostartToggle.Apply(_autostart, on);
            _autostartState = change.State;
            _autostartReadError = null;
            _autostartWriteError = change.Error is { } error ? L.T("settings.autostart_failed", "error", error) : null;
        }
        catch (Exception ex) when (ex is UnauthorizedAccessException or System.Security.SecurityException or IOException)
        {
            // Not even readable: say why, and show it as off.
            _autostartState = AutostartState.Off;
            _autostartReadError = L.T("settings.autostart_failed", "error", ex.Message);
            _autostartWriteError = null;
        }
        Render();
    }

    async void OnOpenOnPlugToggled(object sender, RoutedEventArgs e)
    {
        var on = OpenOnPlugSwitch.IsOn;
        if (_rendering || _settings.Prefs is not { } prefs || prefs.OpenOnPlug == on || _settings.SetPrefsCommand.IsRunning)
        {
            return;
        }
        // The preferences event brings the new value; a failure leaves the sentence in Error.
        await _settings.SetPrefsCommand.ExecuteAsync(prefs with { OpenOnPlug = on });
        Render();
    }

    void OnCopyProxyClick(object sender, RoutedEventArgs e)
    {
        if (Service?.Proxy is { } proxy)
        {
            _copy.Copy(proxy, CopyProxyButton);
        }
    }
}
