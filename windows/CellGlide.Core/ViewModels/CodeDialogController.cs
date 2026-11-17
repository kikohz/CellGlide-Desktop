using System.ComponentModel;

namespace CellGlide.Core.ViewModels;

/// <summary>How a shown code dialog closed (<see cref="ICodeDialogPresenter.ShowAsync"/>).</summary>
public enum CodeDialogClose
{
    /// <summary>"Cancel": the attempt ends.</summary>
    Cancelled,

    /// <summary>Esc, or closed by <see cref="ICodeDialogPresenter.Hide"/>.</summary>
    Dismissed,

    /// <summary>It could not be shown (another dialog is up).</summary>
    NotShown,
}

/// <summary>The code dialog itself (the App's ContentDialog; a fake in the tests).</summary>
public interface ICodeDialogPresenter
{
    /// <summary>The main window is on screen and ready for a dialog.</summary>
    bool CanShow { get; }

    /// <summary>"They Match" was pressed; the dialog stays open.</summary>
    event Action? Matched;

    /// <summary>Shows the dialog with <paramref name="prompt"/>; completes when it closes, or at once when it cannot be shown.</summary>
    Task<CodeDialogClose> ShowAsync(CodePrompt prompt);

    /// <summary>The open dialog shows <paramref name="prompt"/> (confirmed: waiting for the iPhone, Cancel only), and <paramref name="error"/> under it.</summary>
    void Fill(CodePrompt prompt, string? error);

    /// <summary>Closes the open dialog.</summary>
    void Hide();
}

/// <summary>
/// The code dialog's wiring, apart from WinUI: follows <see cref="WirelessViewModel"/> (its code, view and error)
/// through <see cref="CodeDialogTracker"/>, shows, fills and hides the dialog through the presenter, and sends
/// the answers. "They Match" keeps the dialog open as confirmed; when the service refuses that confirmation, the
/// question shows again with the reason. A dialog that could not be shown is tried again on the next change, when
/// asked, when the window comes, and when another dialog closes (<see cref="Retry"/>).
/// </summary>
public sealed class CodeDialogController
{
    readonly WirelessViewModel _wireless;
    readonly ICodeDialogPresenter _presenter;
    readonly CodeDialogTracker _tracker = new();

    public CodeDialogController(WirelessViewModel wireless, ICodeDialogPresenter presenter)
    {
        _wireless = wireless;
        _presenter = presenter;
        wireless.PropertyChanged += OnChanged;
        presenter.Matched += () => Matching = MatchAsync();
    }

    /// <summary>The last dialog shown, until it has ended and been answered (for the tests).</summary>
    internal Task Opening { get; private set; } = Task.CompletedTask;

    /// <summary>The last "They Match", until its reply is handled (for the tests).</summary>
    internal Task Matching { get; private set; } = Task.CompletedTask;

    /// <summary>The prompt the open dialog shows, if one is open.</summary>
    public CodePrompt? Shown => _tracker.Shown;

    /// <summary>"Check the Code": opens the dialog even after Esc put it away.</summary>
    public void Ask()
    {
        _tracker.Ask();
        Follow();
    }

    /// <summary>Something may let a dialog show now: the window came, or another dialog closed.</summary>
    public void Retry() => Follow();

    void OnChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (e.PropertyName is nameof(WirelessViewModel.Code) or nameof(WirelessViewModel.View) or nameof(CoreViewModel.Error))
        {
            Follow();
        }
    }

    void Follow()
    {
        switch (_tracker.Follow(_wireless.Code, _presenter.CanShow))
        {
            case CodeDialogStep.Open:
                Opening = OpenAsync(_tracker.Shown!);
                break;
            case CodeDialogStep.Update:
                _presenter.Fill(_tracker.Shown!, null);
                break;
            case CodeDialogStep.Close:
                _presenter.Hide();
                break;
        }
    }

    async Task OpenAsync(CodePrompt prompt)
    {
        var closed = await _presenter.ShowAsync(prompt);
        var answer = _tracker.Ended(closed != CodeDialogClose.NotShown, closed == CodeDialogClose.Cancelled);
        await _wireless.AnswerCodeAsync(answer);
        if (answer == CodeAnswer.Withdrawn && closed != CodeDialogClose.NotShown)
        {
            // Closed because the phase moved on: a new code may already be waiting.
            Follow();
        }
    }

    async Task MatchAsync()
    {
        if (_tracker.Matched() is not { } confirmed)
        {
            return;
        }
        _presenter.Fill(confirmed, null);
        await _wireless.AnswerCodeAsync(CodeAnswer.Match);
        // Refused (the reply is an error): the question again, with why. The state never said "confirmed", so
        // no change of the code would bring it back by itself.
        if (_wireless.Error is { } error && _tracker.Unconfirmed() is { } again)
        {
            _presenter.Fill(again, error);
        }
    }
}
