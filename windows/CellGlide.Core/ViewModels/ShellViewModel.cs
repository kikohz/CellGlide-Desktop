using System.ComponentModel;
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

    /// <summary>
    /// <see cref="Overview"/>, <see cref="ServiceDown"/> and <see cref="CoreViewModel.Error"/> as one
    /// value that is never null, replaced whenever one of them changes. XAML binds to this: x:Bind does
    /// not call a function again when its argument's path ends in null (the generated code skips it),
    /// so <c>F(Shell.Overview)</c> would keep showing the last state after the service went away.
    /// Rule for the App: pass x:Bind functions only paths that are never null (XBindTests checks it).
    /// </summary>
    public ShellNow Now { get; private set; } = new(null, null, null);

    protected override void OnPropertyChanged(PropertyChangedEventArgs e)
    {
        base.OnPropertyChanged(e);
        if (e.PropertyName is nameof(Overview) or nameof(ServiceDown) or nameof(Error))
        {
            Now = new ShellNow(Overview, ServiceDown, Error);
            base.OnPropertyChanged(new PropertyChangedEventArgs(nameof(Now)));
        }
    }

    public IAsyncRelayCommand ToggleCommand { get; }

    /// <summary>Starts the state events (and the speed and leak pages' watch).</summary>
    public Task SubscribeAsync() => RunAsync(new SubscribeCommand());

    Task ToggleAsync() => RunAsync(new Models.ToggleCommand());
}

/// <summary>What the shell shows now (<see cref="ShellViewModel.Now"/>).</summary>
public sealed record ShellNow(OverviewView? Overview, ServiceGoneEvent? ServiceDown, string? Error)
{
    /// <summary>The state's chrome, or the one that came with <see cref="ServiceDown"/>; null before either.</summary>
    public ChromeView? Chrome => Overview?.Chrome ?? ServiceDown?.Chrome;
}
