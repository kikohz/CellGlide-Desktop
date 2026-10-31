using CellGlide.Core.Dispatch;
using CellGlide.Core.Models;
using CommunityToolkit.Mvvm.Input;

namespace CellGlide.Core.ViewModels;

/// <summary>The checklist (the service's doctor); the only place that runs it.</summary>
public sealed class ChecksViewModel : CoreViewModel
{
    IReadOnlyList<CheckRow>? _rows;

    public ChecksViewModel(EventHub hub)
        : base(hub)
    {
        RunCommand = new AsyncRelayCommand(RunDoctorAsync);
    }

    public IReadOnlyList<CheckRow>? Rows
    {
        get => _rows;
        private set => SetProperty(ref _rows, value);
    }

    public IAsyncRelayCommand RunCommand { get; }

    async Task RunDoctorAsync()
    {
        if (await RunAsync(new RequestCommand(new DoctorRequest())) is ChecksValue checks)
        {
            Rows = checks.Rows;
        }
    }
}
