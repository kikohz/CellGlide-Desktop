using System.ComponentModel;
using CellGlide.App.Hosting;
using CellGlide.App.Shell;
using CellGlide.Core.Desktop;
using CellGlide.Core.Models;
using CellGlide.Core.ViewModels;
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
/// </summary>
public sealed partial class SettingsPage : Page
{
    /// <summary>How long "Copied" shows before turning back into "Copy" (cg-gui COPIED_FOR).</summary>
    static readonly TimeSpan CopiedFor = TimeSpan.FromSeconds(2);

    readonly SettingsViewModel _settings;
    readonly ShellViewModel _shell;
    readonly Autostart _autostart;
    /// <summary>Set while the code itself moves the controls, so their events are not taken for a user's.</summary>
    bool _rendering;
    AutostartState _autostartState;
    // Why reading the Run key failed (cleared by the next read that works), and why the last click's write failed.
    string? _autostartReadError;
    string? _autostartWriteError;
    // "Copied" is the page's own short-lived state; the number lets only the last copy's timer turn it back.
    bool _copied;
    int _copies;

    public SettingsPage(SettingsViewModel settings, ShellViewModel shell, Autostart autostart, Window window)
    {
        _settings = settings;
        _shell = shell;
        _autostart = autostart;
        InitializeComponent();
        Checks.Checks = settings.Checks;
        Checks.RunLabel = L.T("settings.run_checks");

        settings.PropertyChanged += OnChanged;
        shell.PropertyChanged += OnChanged;
        foreach (var command in new INotifyPropertyChanged[]
                 { settings.SetAutoConnectCommand, settings.SetPhoneCommand, settings.SetStrictDnsCommand, settings.SetPrefsCommand })
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
        ReadAutostart();
    }

    SettingsView? Service => _shell.Overview?.Settings;

    void OnChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (e.PropertyName is nameof(ShellViewModel.Now) or nameof(SettingsViewModel.Prefs) or nameof(SettingsViewModel.Error))
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
            PhoneChooser.IsEnabled = service is not null && !_settings.SetPhoneCommand.IsRunning;

            // The service's switches show its values.
            AutoConnectSwitch.IsOn = service?.AutoConnect ?? false;
            AutoConnectSwitch.IsEnabled = service is not null && !_settings.SetAutoConnectCommand.IsRunning;
            StrictDnsSwitch.IsOn = service?.StrictDns ?? false;
            StrictDnsSwitch.IsEnabled = service is not null && !_settings.SetStrictDnsCommand.IsRunning;

            // Start at login: what the Run key says; off and fixed while Windows has turned us off.
            var disabled = _autostartState == AutostartState.DisabledBySystem;
            AutostartSwitch.IsOn = _autostartState == AutostartState.On;
            AutostartSwitch.IsEnabled = !disabled;
            AutomationProperties.SetHelpText(AutostartSwitch, L.T(disabled ? "settings.launch_disabled_by_system" : "settings.launch_note"));
            DisabledBySystemLine.Foreground = Show.Brush(Tone.Orange);
            DisabledBySystemLine.Visibility = Show.Visible(disabled);
            // Never together with the disabled line: while Windows has turned us off, that line is the one that counts.
            var autostartError = disabled ? null : _autostartWriteError ?? _autostartReadError;
            AutostartErrorLine.Text = autostartError ?? "";
            AutostartErrorLine.Foreground = Show.Brush(Tone.Red);
            AutostartErrorLine.Visibility = Show.Visible(autostartError);

            // Open on plug: the app's preference.
            OpenOnPlugSwitch.IsOn = _settings.Prefs?.OpenOnPlug ?? false;
            OpenOnPlugSwitch.IsEnabled = _settings.Prefs is not null && !_settings.SetPrefsCommand.IsRunning;

            // The proxy address, or that there is none yet.
            var proxy = service?.Proxy;
            ProxyText.Text = proxy ?? L.T("settings.proxy_none");
            ProxyText.FontFamily = proxy is null ? FontFamily : new Microsoft.UI.Xaml.Media.FontFamily("Consolas");
            ProxyText.Foreground = proxy is null
                ? (Microsoft.UI.Xaml.Media.Brush)Application.Current.Resources["TextFillColorSecondaryBrush"]
                : (Microsoft.UI.Xaml.Media.Brush)Application.Current.Resources["TextFillColorPrimaryBrush"];
            var copyLabel = L.T(_copied ? "common.copied" : "common.copy");
            CopyProxyButton.Content = copyLabel;
            AutomationProperties.SetName(CopyProxyButton, copyLabel);
            Show.SetEnabled(CopyProxyButton, proxy is not null, Checks);

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

    async void OnCopyProxyClick(object sender, RoutedEventArgs e)
    {
        if (Service?.Proxy is not { } proxy || !Show.Copy(proxy))
        {
            return;
        }
        _copied = true;
        var copy = ++_copies;
        Render();
        Show.Announce(CopyProxyButton);
        await Task.Delay(CopiedFor);
        if (copy == _copies)
        {
            _copied = false;
            Render();
        }
    }
}
