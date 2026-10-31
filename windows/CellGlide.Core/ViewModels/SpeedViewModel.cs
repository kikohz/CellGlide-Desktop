using CellGlide.Core.Dispatch;
using CellGlide.Core.Models;
using CommunityToolkit.Mvvm.Input;

namespace CellGlide.Core.ViewModels;

/// <summary>The speed test page. Dismissing the data warning is <see cref="CancelCommand"/> (SpeedCancel).</summary>
public sealed class SpeedViewModel : CoreViewModel
{
    readonly SettingsViewModel _settings;
    SpeedView? _view;

    public SpeedViewModel(EventHub hub, SettingsViewModel settings)
        : base(hub)
    {
        _settings = settings;
        StartCommand = new AsyncRelayCommand<bool>(StartAsync);
        CancelCommand = new AsyncRelayCommand(() => RunAsync(new SpeedCancelCommand()));
        ClearHistoryCommand = new AsyncRelayCommand(() => RunAsync(new SpeedHistoryClearCommand()));
        hub.SpeedProgress += view => View = view;
    }

    public SpeedView? View
    {
        get => _view;
        private set => SetProperty(ref _view, value);
    }

    /// <summary>
    /// Limits for a small test, sent with every start; null (the default) is the full test. For
    /// development and acceptance runs such as the console probe, never set by the app's UI.
    /// </summary>
    public SpeedCaps? Caps { get; set; }

    /// <summary>Parameter: the user accepted the data warning for this test.</summary>
    public IAsyncRelayCommand<bool> StartCommand { get; }

    public IAsyncRelayCommand CancelCommand { get; }

    public IAsyncRelayCommand ClearHistoryCommand { get; }

    async Task StartAsync(bool confirmed)
    {
        // Wi-Fi is measured or not as the preferences say; read them first if they never came.
        var prefs = _settings.Prefs ?? await _settings.LoadAsync();
        if (prefs is null)
        {
            Error = _settings.Error;
            return;
        }
        await RunAsync(new SpeedStartCommand(prefs.IncludeWifi, confirmed, Caps));
    }
}
