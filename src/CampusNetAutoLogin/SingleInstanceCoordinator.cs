using System.Security.Principal;

namespace CampusNetAutoLogin;

public sealed class SingleInstanceCoordinator : IDisposable
{
    private readonly Mutex _mutex;
    private readonly EventWaitHandle _showSettingsEvent;
    private RegisteredWaitHandle? _registeredWait;
    private bool _ownsMutex;

    public SingleInstanceCoordinator()
    {
        string sid = WindowsIdentity.GetCurrent().User?.Value
            ?? Environment.UserName;
        string suffix = sid.Replace('\\', '_');

        _mutex = new Mutex(
            initiallyOwned: true,
            name: $"Local\\{AppPaths.AppName}.Mutex.{suffix}",
            createdNew: out bool createdNew);
        _ownsMutex = createdNew;

        _showSettingsEvent = new EventWaitHandle(
            initialState: false,
            mode: EventResetMode.AutoReset,
            name: $"Local\\{AppPaths.AppName}.ShowSettings.{suffix}");
    }

    public bool IsFirstInstance => _ownsMutex;

    public void SignalFirstInstance() => _showSettingsEvent.Set();

    public void Listen(Action callback)
    {
        ArgumentNullException.ThrowIfNull(callback);
        if (!IsFirstInstance)
        {
            return;
        }

        _registeredWait = ThreadPool.RegisterWaitForSingleObject(
            _showSettingsEvent,
            (_, timedOut) =>
            {
                if (!timedOut)
                {
                    callback();
                }
            },
            state: null,
            Timeout.InfiniteTimeSpan,
            executeOnlyOnce: false);
    }

    public void Dispose()
    {
        _registeredWait?.Unregister(null);
        _showSettingsEvent.Dispose();

        if (_ownsMutex)
        {
            try
            {
                _mutex.ReleaseMutex();
            }
            catch (ApplicationException)
            {
                // 互斥锁已释放。
            }

            _ownsMutex = false;
        }

        _mutex.Dispose();
    }
}

