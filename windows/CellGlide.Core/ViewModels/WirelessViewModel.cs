using CellGlide.Core.Dispatch;
using CellGlide.Core.Models;
using CommunityToolkit.Mvvm.Input;

namespace CellGlide.Core.ViewModels;

/// <summary>
/// The wireless connection, for the overview, the panel and the settings page: the state's
/// <see cref="WirelessView"/>, the steps card's network name and password (<see cref="Info"/>, asked for while
/// the card is shown and dropped when it goes), the code to confirm (<see cref="Code"/>), and the commands.
/// A failed command leaves its sentence in <see cref="CoreViewModel.Error"/>.
/// </summary>
public sealed class WirelessViewModel : CoreViewModel
{
    WirelessView? _view;
    WirelessSettingsView? _settings;
    WirelessInfoView? _info;
    CodePrompt? _code;
    /// <summary>The steps card was shown with the last state.</summary>
    bool _stepsShown;

    public WirelessViewModel(EventHub hub)
        : base(hub)
    {
        StartCommand = new AsyncRelayCommand(() => RunAsync(new WirelessStartCommand()));
        CancelCommand = new AsyncRelayCommand(() => RunAsync(new WirelessCancelCommand()));
        ConfirmCommand = new AsyncRelayCommand(() => RunAsync(new WirelessConfirmCommand(true)));
        ForgetCommand = new AsyncRelayCommand<string?>(peer => peer is null ? Task.CompletedTask : RunAsync(new WirelessForgetCommand(peer)));
        RepairCommand = new AsyncRelayCommand<string?>(peer => peer is null ? Task.CompletedTask : RunAsync(new WirelessRepairCommand(peer)));
        RegenerateCommand = new AsyncRelayCommand(() => RunAsync(new WirelessRegenerateCommand()));
        // Turning it on checks the access point first: the reply can take half a minute (the DLL waits for it
        // on a connection of its own, and the state keeps coming meanwhile).
        SetEnabledCommand = new AsyncRelayCommand<bool>(on => RunAsync(new RequestCommand(new SetSettingsRequest(null, null, null, on))));
        hub.State += view => Follow(view.Wireless, view.Settings.Wireless);
        hub.ServiceGone += _ => Follow(null, null);
        SetEnabledCommand.PropertyChanged += (_, e) =>
        {
            if (e.PropertyName == nameof(IAsyncRelayCommand.IsRunning))
            {
                OnPropertyChanged(nameof(SwitchBusy));
            }
        };
    }

    /// <summary>The state's wireless part (the button, the attempt), or null.</summary>
    public WirelessView? View
    {
        get => _view;
        private set => SetProperty(ref _view, value);
    }

    /// <summary>The settings page's wireless part, or null without a state.</summary>
    public WirelessSettingsView? Settings
    {
        get => _settings;
        private set => SetProperty(ref _settings, value);
    }

    /// <summary>
    /// The network name, password and steps, while the steps card is shown (asked for when it appears, dropped
    /// when it goes, so the password is held no longer than it is on screen).
    /// </summary>
    public WirelessInfoView? Info
    {
        get => _info;
        private set => SetProperty(ref _info, value);
    }

    /// <summary>The code to compare while pairing, or null.</summary>
    public CodePrompt? Code
    {
        get => _code;
        private set => SetProperty(ref _code, value);
    }

    /// <summary>The switch is on its way or being checked: shown busy, and not sent again.</summary>
    public bool SwitchBusy => SetEnabledCommand.IsRunning || Settings?.Checking == true;

    public IAsyncRelayCommand StartCommand { get; }

    public IAsyncRelayCommand CancelCommand { get; }

    /// <summary>"They Match". Cancelling the code is <see cref="CancelCommand"/>: it ends the attempt.</summary>
    public IAsyncRelayCommand ConfirmCommand { get; }

    /// <summary>Parameter: the paired iPhone's id.</summary>
    public IAsyncRelayCommand<string?> ForgetCommand { get; }

    /// <summary>Parameter: the paired iPhone's id. Off while an attempt runs (<see cref="WirelessSettingsView.Busy"/>).</summary>
    public IAsyncRelayCommand<string?> RepairCommand { get; }

    /// <summary>Off while an attempt runs (<see cref="WirelessSettingsView.Busy"/>).</summary>
    public IAsyncRelayCommand RegenerateCommand { get; }

    /// <summary>Parameter: the switch's new value.</summary>
    public IAsyncRelayCommand<bool> SetEnabledCommand { get; }

    /// <summary>
    /// The user's answer to the code dialog: Match confirms; Cancel ends the attempt (as on the command line);
    /// a dialog put away with Esc, or closed because the phase moved on, sends nothing.
    /// </summary>
    public Task AnswerCodeAsync(CodeAnswer answer) => answer switch
    {
        CodeAnswer.Match => ConfirmCommand.ExecuteAsync(null),
        CodeAnswer.Cancel => CancelCommand.ExecuteAsync(null),
        _ => Task.CompletedTask,
    };

    void Follow(WirelessView? view, WirelessSettingsView? settings)
    {
        View = view;
        Settings = settings;
        Code = view is { Phase: WirelessPhase.Pairing, Code: { } code }
            ? new CodePrompt(code, view.Title, view.Detail, view.Confirmed)
            : null;
        OnPropertyChanged(nameof(SwitchBusy));
        // Asked once each time the steps card appears: a refusal (a read-only client, say) is said once, not again
        // with every state while the card stays.
        var steps = view?.ShowSteps == true;
        if (!steps)
        {
            Info = null;
        }
        else if (!_stepsShown)
        {
            InfoAsked = AskInfoAsync();
        }
        _stepsShown = steps;
    }

    /// <summary>The last ask for the steps card's info (for the tests).</summary>
    internal Task InfoAsked { get; private set; } = Task.CompletedTask;

    /// <summary>The steps card appeared: its network name and password.</summary>
    async Task AskInfoAsync()
    {
        if (await RunAsync(new WirelessInfoCommand()) is WirelessValue value && View?.ShowSteps == true)
        {
            Info = value.View;
        }
    }
}

/// <summary>The code to compare: <c>Code</c> as shown ("384 271"), the dialog's title and sentence, and whether it was confirmed here.</summary>
public sealed record CodePrompt(string Code, string Title, string Detail, bool Confirmed);

/// <summary>How the code dialog ended (<see cref="WirelessViewModel.AnswerCodeAsync"/>).</summary>
public enum CodeAnswer
{
    /// <summary>"They Match".</summary>
    Match,

    /// <summary>"Cancel": the attempt ends.</summary>
    Cancel,

    /// <summary>Put away with Esc: nothing is sent, and it is not shown again by itself for the same code.</summary>
    Dismissed,

    /// <summary>Closed because the phase left pairing (or it could not be shown): nothing is sent.</summary>
    Withdrawn,
}

/// <summary>What the code dialog should do next (<see cref="CodeDialogTracker.Follow"/>).</summary>
public enum CodeDialogStep
{
    None,
    Open,

    /// <summary>The open dialog shows another prompt (confirmed here: waiting for the iPhone, with Cancel only).</summary>
    Update,

    /// <summary>The phase left pairing: the open dialog closes, and its answer is <see cref="CodeAnswer.Withdrawn"/>.</summary>
    Close,
}

/// <summary>
/// The code dialog's life, apart from the dialog itself: one at a time, in the main window only. It opens when a
/// code comes and the window is on screen, shows the prompt as it changes (confirmed here: waiting for the
/// iPhone), and closes when the phase leaves pairing. Put away with Esc, it does not come back by itself for the
/// same code until the user asks (<see cref="Ask"/>: "Check the Code").
/// </summary>
public sealed class CodeDialogTracker
{
    string? _dismissed;
    bool _closing;
    /// <summary>"They Match" was pressed and the service has not answered: the state still asks.</summary>
    bool _matched;

    /// <summary>The prompt the open dialog shows; null while none is open.</summary>
    public CodePrompt? Shown { get; private set; }

    /// <param name="prompt">The code to compare now (<see cref="WirelessViewModel.Code"/>).</param>
    /// <param name="canShow">The main window is on screen and ready for a dialog.</param>
    public CodeDialogStep Follow(CodePrompt? prompt, bool canShow)
    {
        if (_closing)
        {
            // The last dialog is still on its way out: nothing new until it has ended.
            return CodeDialogStep.None;
        }
        if (prompt is null)
        {
            _dismissed = null;
            if (Shown is null)
            {
                return CodeDialogStep.None;
            }
            Shown = null;
            _matched = false;
            _closing = true;
            return CodeDialogStep.Close;
        }
        if (Shown is not null)
        {
            if (Shown == prompt)
            {
                return CodeDialogStep.None;
            }
            if (_matched && prompt == Shown with { Confirmed = false })
            {
                // Confirmed here; the state has not caught up yet (or the reply says it was refused: <see cref="Unconfirmed"/>).
                return CodeDialogStep.None;
            }
            _matched = false;
            Shown = prompt;
            return CodeDialogStep.Update;
        }
        if (!canShow || prompt.Code == _dismissed)
        {
            return CodeDialogStep.None;
        }
        Shown = prompt;
        return CodeDialogStep.Open;
    }

    /// <summary>The user asked to see the code ("Check the Code"): a dialog put away with Esc may open again.</summary>
    public void Ask() => _dismissed = null;

    /// <summary>
    /// "They Match" was pressed: the dialog stays open and shows the code as confirmed (waiting for the iPhone)
    /// until the state says so too. Returns the prompt to show now.
    /// </summary>
    public CodePrompt? Matched()
    {
        if (Shown is { Confirmed: false })
        {
            Shown = Shown with { Confirmed = true };
            _matched = true;
        }
        return Shown;
    }

    /// <summary>
    /// The service refused the confirmation: the open dialog asks again. Returns the prompt to show, or null when
    /// there is nothing to take back (no dialog, or the state already says confirmed).
    /// </summary>
    public CodePrompt? Unconfirmed()
    {
        if (Shown is null || !_matched)
        {
            return null;
        }
        _matched = false;
        Shown = Shown with { Confirmed = false };
        return Shown;
    }

    /// <summary>
    /// The dialog closed: by the user's Cancel, by Esc (<paramref name="cancelled"/> false, <paramref name="shown"/>
    /// true), because it could not be shown (<paramref name="shown"/> false), or because <see cref="Follow"/> closed
    /// it. What to tell the view model.
    /// </summary>
    public CodeAnswer Ended(bool shown, bool cancelled)
    {
        var code = Shown?.Code;
        Shown = null;
        _matched = false;
        if (_closing)
        {
            _closing = false;
            return CodeAnswer.Withdrawn;
        }
        if (!shown)
        {
            return CodeAnswer.Withdrawn;
        }
        if (cancelled)
        {
            return CodeAnswer.Cancel;
        }
        _dismissed = code;
        return CodeAnswer.Dismissed;
    }
}
