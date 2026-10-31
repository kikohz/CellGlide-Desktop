using CellGlide.Core.Dispatch;
using CellGlide.Core.Models;
using CommunityToolkit.Mvvm.Input;

namespace CellGlide.Core.ViewModels;

/// <summary>What the user typed into the diagnostics form.</summary>
public sealed record DiagnosticsInput(DiagRange Range, string Email, string Note);

/// <summary>Where the user chose to save the bundle; <c>Overwrite</c> once they confirmed replacing a file already
/// there (the save dialog asks).</summary>
public sealed record DiagnosticsSaveTarget(string Path, bool Overwrite);

/// <summary>
/// The diagnostics page: prepare the bundle, let the user review it, then send or save it. As in cg-gui
/// pages/diagnostics.rs, only a bundle that was opened in the preview can be sent; saving needs no preview
/// (it prepares the bundle first), but a bundle that was only saved was never shown, so it is not sendable
/// until it is viewed.
/// </summary>
public sealed class DiagnosticsViewModel : CoreViewModel
{
    string? _json;
    string? _fileName;
    string? _code;
    string? _savedPath;
    bool _previewed;
    /// <summary>The form <see cref="Json"/> was built from: a bundle is only used for that same form.</summary>
    DiagnosticsInput? _builtFrom;

    public DiagnosticsViewModel(EventHub hub)
        : base(hub)
    {
        PrepareCommand = new AsyncRelayCommand<DiagnosticsInput?>(PrepareAsync);
        PrepareToSaveCommand = new AsyncRelayCommand<DiagnosticsInput?>(PrepareToSaveAsync);
        SendCommand = new AsyncRelayCommand(SendAsync, () => Json is not null && Previewed);
        SaveCommand = new AsyncRelayCommand<DiagnosticsSaveTarget?>(SaveAsync, _ => Json is not null);
    }

    /// <summary>The bundle on hand: the exact bytes a send or save uses.</summary>
    public string? Json
    {
        get => _json;
        private set
        {
            if (SetProperty(ref _json, value))
            {
                SendCommand.NotifyCanExecuteChanged();
                SaveCommand.NotifyCanExecuteChanged();
            }
        }
    }

    /// <summary><see cref="Json"/> has been opened in the preview: the one condition for sending it.</summary>
    public bool Previewed
    {
        get => _previewed;
        private set
        {
            if (SetProperty(ref _previewed, value))
            {
                SendCommand.NotifyCanExecuteChanged();
            }
        }
    }

    public string? FileName
    {
        get => _fileName;
        private set => SetProperty(ref _fileName, value);
    }

    /// <summary>The report code the server gave back.</summary>
    public string? Code
    {
        get => _code;
        private set => SetProperty(ref _code, value);
    }

    public string? SavedPath
    {
        get => _savedPath;
        private set => SetProperty(ref _savedPath, value);
    }

    /// <summary>"View what will be sent": builds the bundle of the form (unless one is on hand) and marks it previewed.</summary>
    public IAsyncRelayCommand<DiagnosticsInput?> PrepareCommand { get; }

    /// <summary>Before a save: builds the bundle of the form unless one is on hand; it is not previewed by this.</summary>
    public IAsyncRelayCommand<DiagnosticsInput?> PrepareToSaveCommand { get; }

    public IAsyncRelayCommand SendCommand { get; }

    /// <summary>Parameter: where to save, or null for the Downloads folder (never over a file there).</summary>
    public IAsyncRelayCommand<DiagnosticsSaveTarget?> SaveCommand { get; }

    /// <summary>
    /// The form changed (range, email or note): the bundle on hand no longer matches it, so it can be neither
    /// sent nor saved until it is built again (cg-gui pages/diagnostics.rs). What was already saved stays saved.
    /// </summary>
    public void Discard()
    {
        Json = null;
        FileName = null;
        Previewed = false;
        _builtFrom = null;
    }

    /// <summary>"Send Another" (the page keeps the form) and "Done" (the page clears it): forgets the last report.</summary>
    public void Reset()
    {
        Discard();
        Code = null;
        SavedPath = null;
        Error = null;
    }

    async Task PrepareAsync(DiagnosticsInput? input)
    {
        DiscardIfNotFor(input);
        if (Json is not null || await BuildAsync(input))
        {
            Previewed = true;
        }
    }

    async Task PrepareToSaveAsync(DiagnosticsInput? input)
    {
        DiscardIfNotFor(input);
        if (Json is null)
        {
            await BuildAsync(input);
        }
    }

    /// <summary>
    /// A bundle on hand that was built from another form is thrown away, so it is built again for this one. The
    /// page also discards on every change, but a change's event may come after the click that uses the form.
    /// </summary>
    void DiscardIfNotFor(DiagnosticsInput? input)
    {
        if (Json is not null && input != _builtFrom)
        {
            Discard();
        }
    }

    async Task<bool> BuildAsync(DiagnosticsInput? input)
    {
        if (input is null
            || await RunAsync(new DiagnosticsPrepareCommand(input.Range, input.Email, input.Note)) is not DiagnosticsValue bundle)
        {
            return false;
        }
        Previewed = false;
        Json = bundle.Json;
        FileName = bundle.FileName;
        _builtFrom = input;
        Code = null;
        SavedPath = null;
        return true;
    }

    async Task SendAsync()
    {
        if (Json is { } json && Previewed && await RunAsync(new DiagnosticsSendCommand(json)) is SentValue sent)
        {
            Code = sent.Code;
            // A sent bundle is done with: another report needs a new preview (cg-gui pages/diagnostics.rs).
            Discard();
        }
    }

    async Task SaveAsync(DiagnosticsSaveTarget? target)
    {
        if (Json is { } json
            && await RunAsync(new DiagnosticsSaveCommand(json, target?.Path, target?.Overwrite ?? false)) is SavedValue saved)
        {
            SavedPath = saved.Path;
        }
    }
}
