using CellGlide.Core.Dispatch;
using CellGlide.Core.Models;
using CommunityToolkit.Mvvm.Input;

namespace CellGlide.Core.ViewModels;

/// <summary>
/// The settings page. The service's settings are shown from the state's settings view
/// (<see cref="ShellViewModel.Overview"/>); the app's own preferences are <see cref="Prefs"/>.
/// </summary>
public sealed class SettingsViewModel : CoreViewModel
{
    Prefs? _prefs;

    public SettingsViewModel(EventHub hub)
        : base(hub)
    {
        SetAutoConnectCommand = new AsyncRelayCommand<bool>(on => SetAsync(new SetSettingsRequest(on, null, null)));
        SetPhoneCommand = new AsyncRelayCommand<PhoneChoice?>(SetPhoneAsync);
        SetStrictDnsCommand = new AsyncRelayCommand<bool>(on => SetAsync(new SetSettingsRequest(null, null, on)));
        SetPrefsCommand = new AsyncRelayCommand<Prefs?>(SetPrefsAsync);
        Checks = new ChecksViewModel(hub);
        hub.PrefsChanged += prefs => Prefs = prefs;
    }

    /// <summary>The app's preferences, once read (<see cref="LoadAsync"/>) or changed.</summary>
    public Prefs? Prefs
    {
        get => _prefs;
        private set => SetProperty(ref _prefs, value);
    }

    public IAsyncRelayCommand<bool> SetAutoConnectCommand { get; }

    public IAsyncRelayCommand<PhoneChoice?> SetPhoneCommand { get; }

    public IAsyncRelayCommand<bool> SetStrictDnsCommand { get; }

    public IAsyncRelayCommand<Prefs?> SetPrefsCommand { get; }

    public ChecksViewModel Checks { get; }

    /// <summary>Reads the preferences: them, or null with <see cref="CoreViewModel.Error"/> set.</summary>
    public async Task<Prefs?> LoadAsync()
    {
        if (await RunAsync(new GetPrefsCommand()) is PrefsValue value)
        {
            Prefs = value.Prefs;
        }
        return Prefs;
    }

    Task SetAsync(SetSettingsRequest request) => RunAsync(new RequestCommand(request));

    Task SetPhoneAsync(PhoneChoice? choice) =>
        choice is null ? Task.CompletedTask : SetAsync(new SetSettingsRequest(null, choice, null));

    Task SetPrefsAsync(Prefs? prefs) =>
        prefs is null ? Task.CompletedTask : RunAsync(new Models.SetPrefsCommand(prefs));
}
