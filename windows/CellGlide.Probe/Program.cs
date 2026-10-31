// CellGlide.Probe: drives cellglide_native.dll through CellGlide.Core from a console.
//
//   dotnet run --project CellGlide.Probe -- status [--seconds N] [--lang TAG]
//   dotnet run --project CellGlide.Probe -- speed-small [--lang TAG]
//   dotnet run --project CellGlide.Probe -- leak [--lang TAG]
//
// The commands themselves are in ProbeRunner. Here: the command line, the DLL, and Ctrl+C.
using System.Text;
using CellGlide.Core.Dispatch;
using CellGlide.Core.Native;

namespace CellGlide.Probe;

static class Program
{
    static async Task<int> Main(string[] args)
    {
        Console.OutputEncoding = Encoding.UTF8;
        if (ProbeOptions.Parse(args) is not { } options)
        {
            Console.Error.WriteLine(ProbeOptions.Usage);
            return ProbeRunner.Trouble;
        }

        // Ctrl+C (and, as a fallback, the process being told to exit) only signals `stop`: the
        // runner cancels a running test and returns, and the `using`s below then dispose the hub
        // and the core, so cg_shutdown always runs. Registered before the DLL starts, so this
        // ProcessExit handler runs ahead of NativeCore's own (which calls cg_shutdown).
        using var stop = new CancellationTokenSource();
        void Signal()
        {
            try
            {
                stop.Cancel();
            }
            catch (ObjectDisposedException)
            {
                // Main has already returned.
            }
        }
        Console.CancelKeyPress += (_, e) =>
        {
            e.Cancel = true;
            Signal();
        };
        AppDomain.CurrentDomain.ProcessExit += (_, _) => Signal();

        using var core = NativeCore.Start(options.Lang);
        using var hub = new EventHub(core, new InlineDispatcher());
        return await new ProbeRunner(hub, Console.Out).RunAsync(options, stop.Token);
    }
}
