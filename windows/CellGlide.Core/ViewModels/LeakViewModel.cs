using CellGlide.Core.Dispatch;
using CellGlide.Core.Models;
using CommunityToolkit.Mvvm.Input;

namespace CellGlide.Core.ViewModels;

/// <summary>The DNS leak test page.</summary>
public sealed class LeakViewModel : CoreViewModel
{
    LeakView? _view;

    public LeakViewModel(EventHub hub)
        : base(hub)
    {
        StartCommand = new AsyncRelayCommand(() => RunAsync(new LeakStartCommand()));
        CancelCommand = new AsyncRelayCommand(() => RunAsync(new LeakCancelCommand()));
        Checks = new ChecksViewModel(hub);
        hub.LeakProgress += view => View = view;
    }

    public LeakView? View
    {
        get => _view;
        private set => SetProperty(ref _view, value);
    }

    public IAsyncRelayCommand StartCommand { get; }

    public IAsyncRelayCommand CancelCommand { get; }

    /// <summary>The checklist, offered when the view says so.</summary>
    public ChecksViewModel Checks { get; }
}
