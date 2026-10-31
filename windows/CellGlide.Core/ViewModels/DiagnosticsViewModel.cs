using CellGlide.Core.Dispatch;
using CellGlide.Core.Models;
using CommunityToolkit.Mvvm.Input;

namespace CellGlide.Core.ViewModels;

/// <summary>What the user typed into the diagnostics form.</summary>
public sealed record DiagnosticsInput(DiagRange Range, string Email, string Note);

/// <summary>The diagnostics page: prepare the bundle, let the user review it, then send or save it.</summary>
public sealed class DiagnosticsViewModel : CoreViewModel
{
    string? _json;
    string? _fileName;
    string? _code;
    string? _savedPath;

    public DiagnosticsViewModel(EventHub hub)
        : base(hub)
    {
        PrepareCommand = new AsyncRelayCommand<DiagnosticsInput?>(PrepareAsync);
        SendCommand = new AsyncRelayCommand(SendAsync, () => Json is not null);
        SaveCommand = new AsyncRelayCommand<string?>(SaveAsync, _ => Json is not null);
    }

    /// <summary>The bundle for the user to review.</summary>
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

    public IAsyncRelayCommand<DiagnosticsInput?> PrepareCommand { get; }

    public IAsyncRelayCommand SendCommand { get; }

    /// <summary>Parameter: where to save, or null for the Downloads folder.</summary>
    public IAsyncRelayCommand<string?> SaveCommand { get; }

    async Task PrepareAsync(DiagnosticsInput? input)
    {
        if (input is null)
        {
            return;
        }
        if (await RunAsync(new DiagnosticsPrepareCommand(input.Range, input.Email, input.Note)) is DiagnosticsValue bundle)
        {
            Json = bundle.Json;
            FileName = bundle.FileName;
            Code = null;
            SavedPath = null;
        }
    }

    async Task SendAsync()
    {
        if (Json is { } json && await RunAsync(new DiagnosticsSendCommand(json)) is SentValue sent)
        {
            Code = sent.Code;
        }
    }

    async Task SaveAsync(string? path)
    {
        if (Json is { } json && await RunAsync(new DiagnosticsSaveCommand(json, path)) is SavedValue saved)
        {
            SavedPath = saved.Path;
        }
    }
}
