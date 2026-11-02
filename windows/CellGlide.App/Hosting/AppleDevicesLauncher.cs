using System.Runtime.InteropServices;
using CellGlide.Core.Desktop;

namespace CellGlide.App.Hosting;

/// <summary>
/// Starts Apple Devices (the Store package AppleInc.AppleDevices_nzyj5cx40ttqa) as the signed-in user, each call
/// on its own COM thread.
/// <para>
/// In the background: the package declares an out-of-process COM server, AMPDevicesAgent.exe, for the class
/// AMP.Devices.IDiscovery. Creating that class starts AMPDevicesAgent, which starts AppleMobileDeviceLauncher and
/// with it AppleMobileDeviceProcess (usbmuxd, 127.0.0.1:27015), with no window. Measured on Windows 11 26200 with
/// Apple Devices 1.1792.39682.0 (W4 Task 13): listening about 1.5 s after the call, no new top-level window.
/// Activating the AMPDevicesAgent application itself (IApplicationActivationManager) returns S_OK but the agent
/// exits at once and nothing listens, so it is not used. Started within about 10 s of the previous usbmuxd
/// ending, the new one may never list the iPhone; AppleDevicesStarter waits before its first attempt for that.
/// </para>
/// <para>The window: IApplicationActivationManager activates the package's App (AppleDevices.exe).</para>
/// </summary>
public sealed unsafe partial class AppleDevicesLauncher : IAppleDevicesLauncher
{
    const string AppId = "AppleInc.AppleDevices_nzyj5cx40ttqa!App";

    /// <summary>AMP.Devices.IDiscovery, served by AMPDevicesAgent.exe (the package's windows.comServer).</summary>
    static readonly Guid AgentClass = new("054AA48D-7681-4C3C-9554-AE093D8798DB");

    static readonly Guid ActivationManagerClass = new("45BA127D-10A8-46EA-8AB7-56EA9078943C");
    static readonly Guid IApplicationActivationManager = new("2E941141-7F97-4756-BA1D-9DECDE894A3D");
    static readonly Guid IUnknown = new("00000000-0000-0000-C000-000000000046");

    const uint CLSCTX_INPROC_SERVER = 0x1;
    const uint CLSCTX_LOCAL_SERVER = 0x4;
    const uint COINIT_APARTMENTTHREADED = 0x2;
    const int RPC_E_CHANGED_MODE = unchecked((int)0x80010106);

    public Task StartInBackgroundAsync() => OnComThread(() =>
    {
        Marshal.ThrowExceptionForHR(CoCreateInstance(AgentClass, 0, CLSCTX_LOCAL_SERVER, IUnknown, out var agent));
        // Only its starting matters: the agent keeps running (and usbmuxd with it) once let go.
        Marshal.Release(agent);
    });

    public Task OpenAsync() => OnComThread(() =>
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
    });

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
