using CellGlide.Core.Dispatch;
using CellGlide.Core.Models;
using CommunityToolkit.Mvvm.Input;

namespace CellGlide.Core.ViewModels;

/// <summary>The connection history page.</summary>
public sealed class HistoryViewModel : CoreViewModel
{
    HistoryView? _view;

    public HistoryViewModel(EventHub hub)
        : base(hub)
    {
        LoadCommand = new AsyncRelayCommand<string?>(LoadAsync);
    }

    public HistoryView? View
    {
        get => _view;
        private set => SetProperty(ref _view, value);
    }

    /// <summary>Parameter: the day (YYYY-MM-DD, as the view's prev_day and next_day give it), null for today.</summary>
    public IAsyncRelayCommand<string?> LoadCommand { get; }

    async Task LoadAsync(string? day)
    {
        if (await RunAsync(new RequestCommand(new HistoryRequest(day))) is HistoryValue history)
        {
            View = history.View;
        }
    }
}
