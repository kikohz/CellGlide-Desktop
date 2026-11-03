using System.Diagnostics;
using System.Runtime.InteropServices;
using CellGlide.Core.Desktop;

namespace CellGlide.App.Hosting;

/// <summary>
/// Starts Apple Devices (the Store package AppleInc.AppleDevices_nzyj5cx40ttqa) as the signed-in user.
/// <para>
/// In the background: what the package's startup task does at sign-in, run AppleMobileDeviceLauncher.exe (which
/// starts AppleMobileDeviceProcess, usbmuxd on 127.0.0.1:27015) inside the package, through Windows'
/// Invoke-CommandInDesktopPackage in a PowerShell that has no window. Measured on Windows 11 26200 with Apple
/// Devices 1.1792.39682.0 (W4 Task 13): listening 4-6 s after the call, the iPhone found, no new top-level
/// window. Not used, from the same measurements: activating the package's AMPDevicesAgent application exits at
/// once and starts nothing; creating its COM class (AMP.Devices.IDiscovery) starts the agent, the launcher and
/// usbmuxd with no window, but that usbmuxd often never lists the iPhone that is plugged in (6 of 8 times).
/// </para>
/// <para>The window: IApplicationActivationManager activates the package's App (AppleDevices.exe), on its own COM thread.</para>
/// </summary>
public sealed partial class AppleDevicesLauncher : IAppleDevicesLauncher
{
    const string AppId = "AppleInc.AppleDevices_nzyj5cx40ttqa!App";

    /// <summary>
    /// The background start, as PowerShell runs it; it throws (exit code 1) when it can't. The Appx module is the
    /// system's own (by path, and its commands called by module name, so a module of the same name elsewhere on
    /// PSModulePath isn't used); the package must be the Store's Apple Devices for this user, installed under
    /// %ProgramFiles%\WindowsApps\ or, where Settings sends new apps to another drive, that drive's
    /// &lt;drive&gt;:\WindowsApps\ (not a registered loose copy in a folder the user can write). No -ExecutionPolicy: a -Command
    /// string isn't subject to it, and the signed system module loads under Restricted too (checked on this PC).
    /// </summary>
    const string StartScript =
        "$ErrorActionPreference = 'Stop'; " +
        "Import-Module (Join-Path $env:SystemRoot 'System32\\WindowsPowerShell\\v1.0\\Modules\\Appx\\Appx.psd1'); " +
        "$p = Appx\\Get-AppxPackage -Name 'AppleInc.AppleDevices' | Where-Object { $_.PackageFamilyName -eq 'AppleInc.AppleDevices_nzyj5cx40ttqa' -and $_.SignatureKind -eq 'Store' } | Select-Object -First 1; " +
        "if (-not $p) { throw 'Apple Devices from the Store is not installed for this user' }; " +
        "$at = $p.InstallLocation; $apps = Join-Path $env:ProgramFiles 'WindowsApps\\'; " +
        "if ($at -match '\\.\\.' -or -not ($at.StartsWith($apps, [StringComparison]::OrdinalIgnoreCase) -or $at -match '^[A-Za-z]:\\\\WindowsApps\\\\')) { throw ('Apple Devices is not under ' + $apps + ' or <drive>:\\WindowsApps\\: ' + $at) }; " +
        "Appx\\Invoke-CommandInDesktopPackage -PackageFamilyName $p.PackageFamilyName -AppId 'AMPDevicesAgent' -Command (Join-Path $p.InstallLocation 'AppleMobileDeviceLauncher.exe')";

    /// <summary>The longest error text kept for the log (one line).</summary>
    const int MaxErrorText = 500;

    /// <summary>How long the PowerShell that starts it may take.</summary>
    static readonly TimeSpan StartTimeout = TimeSpan.FromSeconds(60);

    static readonly Guid ActivationManagerClass = new("45BA127D-10A8-46EA-8AB7-56EA9078943C");
    static readonly Guid IApplicationActivationManager = new("2E941141-7F97-4756-BA1D-9DECDE894A3D");

    const uint CLSCTX_INPROC_SERVER = 0x1;
    const uint CLSCTX_LOCAL_SERVER = 0x4;
    const uint COINIT_APARTMENTTHREADED = 0x2;
    const int RPC_E_CHANGED_MODE = unchecked((int)0x80010106);

    public async Task StartInBackgroundAsync()
    {
        var start = new ProcessStartInfo(Path.Combine(Environment.SystemDirectory, @"WindowsPowerShell\v1.0\powershell.exe"))
        {
            UseShellExecute = false,
            // No console at all, so nothing shows (not even a terminal the console would be handed to).
            CreateNoWindow = true,
            RedirectStandardOutput = true,
            RedirectStandardError = true,
        };
        foreach (var argument in new[] { "-NoProfile", "-NonInteractive", "-Command", StartScript })
        {
            start.ArgumentList.Add(argument);
        }
        // Off the UI thread: starting a process can take a while (an antivirus scan, say).
        using var process = await Task.Run(() => Process.Start(start)) ?? throw new InvalidOperationException("PowerShell did not start");
        var output = process.StandardOutput.ReadToEndAsync();
        var errors = process.StandardError.ReadToEndAsync();
        using var timeout = new CancellationTokenSource(StartTimeout);
        try
        {
            await process.WaitForExitAsync(timeout.Token);
        }
        catch (OperationCanceledException)
        {
            try
            {
                process.Kill(entireProcessTree: true);
            }
            catch (Exception)
            {
                // It may have ended meanwhile.
            }
            throw new TimeoutException($"PowerShell took longer than {StartTimeout.TotalSeconds:0} s");
        }
        await output;
        var said = FileLog.OneLine(await errors, MaxErrorText);
        if (process.ExitCode != 0)
        {
            throw new InvalidOperationException($"PowerShell exit code {process.ExitCode}: {said}");
        }
    }

    /// <summary>Whether something listens on 127.0.0.1:27015 (the TCP table only; nothing connects).</summary>
    public bool UsbmuxdListening() =>
        System.Net.NetworkInformation.IPGlobalProperties.GetIPGlobalProperties().GetActiveTcpListeners()
            .Any(e => e.Port == 27015 && (e.Address.Equals(System.Net.IPAddress.Loopback) || e.Address.Equals(System.Net.IPAddress.Any)));

    public Task OpenAsync() => OnComThread(Activate);

    static unsafe void Activate()
    {
        Marshal.ThrowExceptionForHR(CoCreateInstance(ActivationManagerClass, 0, CLSCTX_INPROC_SERVER | CLSCTX_LOCAL_SERVER, IApplicationActivationManager, out var manager));
        try
        {
            // IApplicationActivationManager::ActivateApplication, the first method after IUnknown's three.
            var activate = (delegate* unmanaged[Stdcall]<nint, char*, char*, int, uint*, int>)(*(void***)manager)[3];
            uint process;
            int hr;
            fixed (char* id = AppId)
            {
                hr = activate(manager, id, null, 0, &process);
            }
            Marshal.ThrowExceptionForHR(hr);
        }
        finally
        {
            Marshal.Release(manager);
        }
    }

    /// <summary>Runs <paramref name="action"/> on a new single-threaded COM apartment; the task ends with it.</summary>
    static Task OnComThread(Action action)
    {
        var done = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        var thread = new Thread(() =>
        {
            var init = CoInitializeEx(0, COINIT_APARTMENTTHREADED);
            try
            {
                if (init < 0 && init != RPC_E_CHANGED_MODE)
                {
                    Marshal.ThrowExceptionForHR(init);
                }
                action();
                done.SetResult();
            }
            catch (Exception e)
            {
                done.SetException(e);
            }
            finally
            {
                if (init >= 0)
                {
                    CoUninitialize();
                }
            }
        })
        {
            IsBackground = true,
            Name = "Apple Devices",
        };
        thread.Start();
        return done.Task;
    }

    [LibraryImport("ole32.dll")]
    private static partial int CoInitializeEx(nint reserved, uint coInit);

    [LibraryImport("ole32.dll")]
    private static partial void CoUninitialize();

    [LibraryImport("ole32.dll")]
    private static partial int CoCreateInstance(in Guid clsid, nint outer, uint context, in Guid iid, out nint instance);
}
