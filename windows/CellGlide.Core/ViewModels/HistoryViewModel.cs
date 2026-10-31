using CellGlide.Core.Dispatch;
using CellGlide.Core.Models;
using CommunityToolkit.Mvvm.Input;

namespace CellGlide.Core.ViewModels;

/// <summary>
/// The connection history page. Following today (<see cref="RequestedDay"/> null) it is refreshed every
/// few seconds by the page; another day is left as it is (cg-gui pages/history.rs, refresh_only_for_today).
/// </summary>
public sealed class HistoryViewModel : CoreViewModel
{
    HistoryView? _view;
    string? _requestedDay;
    // Each load's number; only the newest load's answer is shown, so a refresh of today that comes back
    // after the user paged to another day never replaces it.
    long _loads;

    public HistoryViewModel(EventHub hub)
        : base(hub)
    {
        LoadCommand = new AsyncRelayCommand<string?>(LoadAsync, AsyncRelayCommandOptions.AllowConcurrentExecutions);
        RefreshCommand = new AsyncRelayCommand(RefreshAsync);
    }

    public HistoryView? View
    {
        get => _view;
        private set => SetProperty(ref _view, value);
    }

    /// <summary>
    /// The day last asked for with <see cref="LoadCommand"/>, null for today. Landing on the newest day
    /// on record (the view has no next day) follows today again, so midnight moves it along.
    /// </summary>
    public string? RequestedDay
    {
        get => _requestedDay;
        private set => SetProperty(ref _requestedDay, value);
    }

    /// <summary>Parameter: the day (YYYY-MM-DD, as the view's prev_day and next_day give it), null for today.</summary>
    public IAsyncRelayCommand<string?> LoadCommand { get; }

    /// <summary>Asks for today again, only while following today and when no load is out; otherwise nothing.</summary>
    public IAsyncRelayCommand RefreshCommand { get; }

    async Task LoadAsync(string? day)
    {
        var load = ++_loads;
        RequestedDay = day;
        var reply = await RunAsync(new RequestCommand(new HistoryRequest(day)));
        if (load != _loads)
        {
            // The user asked for another day meanwhile; its answer is the one to show.
            return;
        }
        if (reply is HistoryValue history)
        {
            View = history.View;
            if (day is not null && history.View.Day == day && history.View.NextDay is null)
            {
                RequestedDay = null;
            }
        }
    }

    Task RefreshAsync() =>
        RequestedDay is null && !LoadCommand.IsRunning ? LoadCommand.ExecuteAsync(null) : Task.CompletedTask;
}
