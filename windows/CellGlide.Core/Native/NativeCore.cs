using System.Runtime.CompilerServices;
using System.Runtime.InteropServices;
using System.Text;
using System.Text.Json;
using CellGlide.Core.Dispatch;
using CellGlide.Core.Models;

namespace CellGlide.Core.Native;

/// <summary>
/// cellglide_native.dll, through the csbindgen bindings in NativeMethods.g.cs. There is one
/// runtime in the DLL: start one NativeCore at a time and dispose it before starting another.
/// Events arrive on the DLL's own thread (see <see cref="ICore.EventReceived"/>); hand them to the
/// UI thread with <see cref="EventHub"/>. Dispose before the process exits (a ProcessExit handler
/// does it if nobody else has).
/// </summary>
public sealed unsafe class NativeCore : ICore
{
    GCHandle _self;
    int _disposed;

    NativeCore()
    {
    }

    public event Action<CoreEvent>? EventReceived;

    /// <summary>Starts the DLL in <paramref name="lang"/> (a tag such as zh-CN; empty for the system's language).</summary>
    /// <exception cref="InvalidOperationException">cg_init refused, for example because the DLL is already running.</exception>
    public static NativeCore Start(string lang)
    {
        var core = new NativeCore();
        core._self = GCHandle.Alloc(core);
        var bytes = Encoding.UTF8.GetBytes(lang);
        int code;
        fixed (byte* p = bytes)
        {
            code = NativeMethods.cg_init(p, (nuint)bytes.Length, &OnEvent, (void*)GCHandle.ToIntPtr(core._self));
        }
        if (code != NativeMethods.CG_OK)
        {
            core._self.Free();
            throw new InvalidOperationException($"cg_init returned {code}");
        }
        AppDomain.CurrentDomain.ProcessExit += core.OnProcessExit;
        return core;
    }

    public ulong Send(Command command)
    {
        if (Volatile.Read(ref _disposed) != 0)
        {
            return 0;
        }
        var json = JsonSerializer.SerializeToUtf8Bytes(command, CoreJsonContext.Default.Command);
        fixed (byte* p = json)
        {
            return NativeMethods.cg_send(p, (nuint)json.Length);
        }
    }

    public string Tr(string key, IReadOnlyDictionary<string, string>? args = null)
    {
        var keyBytes = Encoding.UTF8.GetBytes(key);
        var argBytes = args is { Count: > 0 }
            ? JsonSerializer.SerializeToUtf8Bytes(new Dictionary<string, string>(args), CoreJsonContext.Default.DictionaryStringString)
            : [];
        byte* text = null;
        nuint length = 0;
        fixed (byte* k = keyBytes)
        fixed (byte* a = argBytes)
        {
            NativeMethods.cg_tr(k, (nuint)keyBytes.Length, a, (nuint)argBytes.Length, &text, &length);
        }
        if (text == null)
        {
            // Only after a panic inside the DLL; the key at least says what was asked for.
            return key;
        }
        try
        {
            return Encoding.UTF8.GetString(text, checked((int)length));
        }
        finally
        {
            NativeMethods.cg_free(text, length);
        }
    }

    /// <summary>Stops the DLL (cg_shutdown): once this returns, no more events.</summary>
    public void Dispose()
    {
        if (Interlocked.Exchange(ref _disposed, 1) != 0)
        {
            return;
        }
        AppDomain.CurrentDomain.ProcessExit -= OnProcessExit;
        NativeMethods.cg_shutdown();
        // No callback can use the handle once cg_shutdown has returned (or, from inside the
        // callback, once that callback returns; it already holds its target).
        _self.Free();
    }

    void OnProcessExit(object? sender, EventArgs e) => Dispose();

    [UnmanagedCallersOnly(CallConvs = [typeof(CallConvCdecl)])]
    static void OnEvent(void* ctx, byte* json, nuint len)
    {
        // Nothing may escape into the DLL: every failure becomes a Fault or is dropped.
        try
        {
            if (GCHandle.FromIntPtr((nint)ctx).Target is not NativeCore core)
            {
                return;
            }
            CoreEvent ev;
            try
            {
                var bytes = new ReadOnlySpan<byte>(json, checked((int)len)).ToArray();
                ev = JsonSerializer.Deserialize(bytes, CoreJsonContext.Default.CoreEvent)
                    ?? throw new JsonException("null event");
            }
            catch (Exception e)
            {
                ev = new FaultEvent(e.Message);
            }
            core.Raise(ev);
        }
        catch
        {
        }
    }

    void Raise(CoreEvent ev)
    {
        try
        {
            EventReceived?.Invoke(ev);
        }
        catch (Exception e) when (ev is not FaultEvent)
        {
            EventReceived?.Invoke(new FaultEvent(e.Message));
        }
    }
}
