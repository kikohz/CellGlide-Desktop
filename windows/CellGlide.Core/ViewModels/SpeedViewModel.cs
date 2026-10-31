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
        hub.SpeedProgress += OnProgress;
    }

    /// <summary>
    /// The DLL put the data warning up: raised once when <see cref="SpeedView.Dialog"/> becomes
    /// <see cref="SpeedDialog.DataWarning"/>, not again for later frames of the same warning. The
    /// page asks the user; the answer is <see cref="StartCommand"/> with true, or <see cref="CancelCommand"/>.
    /// </summary>
    public event Action? DataWarningRequested;

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

    /// <summary>
    /// The user's answer to the data warning. Continue: with <paramref name="dontAskAgain"/>, first saves
    /// the preference not to ask again (SetPrefs), then starts the test confirmed (whether or not the
    /// preference could be saved; a failure stays in the settings' Error). Dismiss (Cancel or Esc):
    /// SpeedCancel, which puts the warning away in the DLL; the box is ignored. Withdrawn: the DLL put
    /// it away by itself (the phone left), so nothing is sent.
    /// </summary>
    public async Task AnswerDataWarningAsync(DataWarningAnswer choice, bool dontAskAgain)
    {
        switch (choice)
        {
            case DataWarningAnswer.Continue:
                if (dontAskAgain && (_settings.Prefs ?? await _settings.LoadAsync()) is { } prefs)
                {
                    await _settings.SetPrefsCommand.ExecuteAsync(prefs with { SpeedConfirmed = true });
                }
                await StartCommand.ExecuteAsync(true);
                break;
            case DataWarningAnswer.Dismiss:
                await CancelCommand.ExecuteAsync(null);
                break;
            case DataWarningAnswer.Withdrawn:
                break;
        }
    }

    void OnProgress(SpeedView view)
    {
        var shown = _view?.Dialog == SpeedDialog.DataWarning;
        View = view;
        if (!shown && view.Dialog == SpeedDialog.DataWarning)
        {
            DataWarningRequested?.Invoke();
        }
    }
}

/// <summary>How the data warning was answered (<see cref="SpeedViewModel.AnswerDataWarningAsync"/>).</summary>
public enum DataWarningAnswer
{
    /// <summary>The user chose to go on.</summary>
    Continue,

    /// <summary>The user said no: Cancel, Esc, or the dialog could not be shown.</summary>
    Dismiss,

    /// <summary>The DLL put the warning away by itself before the user answered.</summary>
    Withdrawn,
}
