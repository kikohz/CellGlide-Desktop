using CellGlide.Core.Dispatch;
using CellGlide.Core.Models;
using CommunityToolkit.Mvvm.Input;

namespace CellGlide.Core.ViewModels;

/// <summary>The window and tray: the latest state, or why there is none.</summary>
public sealed class ShellViewModel : CoreViewModel
{
    OverviewView? _overview;
    ServiceGoneEvent? _serviceDown;

    public ShellViewModel(EventHub hub)
        : base(hub)
    {
        ToggleCommand = new AsyncRelayCommand(ToggleAsync);
        hub.State += view =>
        {
            Overview = view;
            ServiceDown = null;
        };
        hub.ServiceGone += gone =>
        {
            // The last state no longer holds; the tray, menu, sidebar and dots come with the event.
            ServiceDown = gone;
            Overview = null;
        };
    }

    public OverviewView? Overview
    {
        get => _overview;
        private set
        {
            if (SetProperty(ref _overview, value))
            {
                OnPropertyChanged(nameof(Chrome));
            }
        }
    }

    /// <summary>Why there is no state, with the chrome to show meanwhile; null while there is state.</summary>
    public ServiceGoneEvent? ServiceDown
    {
        get => _serviceDown;
        private set
        {
            if (SetProperty(ref _serviceDown, value))
            {
                OnPropertyChanged(nameof(Chrome));
            }
        }
    }

    /// <summary>
    /// The window title, tray, menu, sidebar and dots to show now: the state's, or the ones that came
    /// with <see cref="ServiceDown"/>; null before either has arrived.
    /// </summary>
    public ChromeView? Chrome => Overview?.Chrome ?? ServiceDown?.Chrome;

    public IAsyncRelayCommand ToggleCommand { get; }

    /// <summary>Starts the state events (and the speed and leak pages' watch).</summary>
    public Task SubscribeAsync() => RunAsync(new SubscribeCommand());

    Task ToggleAsync() => RunAsync(new Models.ToggleCommand());
}
