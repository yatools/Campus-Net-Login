using System.Runtime.InteropServices;
using System.Security;
using System.Security.Principal;
using System.Xml;
using System.Xml.Linq;

namespace CampusNetAutoLogin;

public sealed class StartupManager
{
    private const int TaskCreateOrUpdate = 6;
    private const int TaskLogonInteractiveToken = 3;

    private readonly string _executablePath;
    private readonly string _taskName;

    public StartupManager(string? executablePath = null)
        : this(executablePath, taskNameOverride: null)
    {
    }

    internal StartupManager(string? executablePath, string? taskNameOverride)
    {
        _executablePath = executablePath ?? AppPaths.ExecutablePath;
        string sid = WindowsIdentity.GetCurrent().User?.Value
            ?? Environment.UserName;
        _taskName = taskNameOverride ?? $"{AppPaths.AppName}-{sid}";
    }

    public string TaskName => _taskName;

    public Task<bool> IsEnabledAsync() => Task.Run(IsEnabledCore);

    public Task EnableAsync() => Task.Run(EnableCore);

    public Task DisableAsync() => Task.Run(DisableCore);

    private bool IsEnabledCore()
    {
        ComScope scope = new();
        try
        {
            ITaskService service = scope.Track(CreateService());
            service.Connect(null, null, null, null);
            ITaskFolder root = scope.Track(service.GetFolder("\\"));
            IRegisteredTask task = scope.Track(root.GetTask(_taskName));
            if (!task.GetEnabled())
            {
                return false;
            }

            XDocument document = XDocument.Parse(task.GetXml());
            XNamespace taskNamespace =
                "http://schemas.microsoft.com/windows/2004/02/mit/task";
            string? configuredPath = document
                .Descendants(taskNamespace + "Command")
                .FirstOrDefault()
                ?.Value;

            return !string.IsNullOrWhiteSpace(configuredPath) &&
                   PathsEqual(configuredPath, _executablePath);
        }
        catch (COMException exception) when (IsTaskNotFound(exception))
        {
            return false;
        }
        catch (DirectoryNotFoundException)
        {
            return false;
        }
        catch (FileNotFoundException)
        {
            return false;
        }
        catch (XmlException)
        {
            return false;
        }
        finally
        {
            scope.Dispose();
        }
    }

    private void EnableCore()
    {
        WindowsIdentity identity = WindowsIdentity.GetCurrent();
        string userName = identity.Name;
        string userSid = identity.User?.Value
            ?? throw new InvalidOperationException("无法确定当前 Windows 用户 SID。");
        string workingDirectory =
            Path.GetDirectoryName(_executablePath) ?? AppContext.BaseDirectory;
        string taskXml = BuildTaskXml(
            userName,
            userSid,
            workingDirectory);

        ComScope scope = new();
        try
        {
            ITaskService service = scope.Track(CreateService());
            service.Connect(null, null, null, null);
            ITaskFolder root = scope.Track(service.GetFolder("\\"));
            IRegisteredTask task = scope.Track(root.RegisterTask(
                _taskName,
                taskXml,
                TaskCreateOrUpdate,
                userName,
                null,
                TaskLogonInteractiveToken,
                null));
            _ = task;
        }
        finally
        {
            scope.Dispose();
        }
    }

    private void DisableCore()
    {
        ComScope scope = new();
        try
        {
            ITaskService service = scope.Track(CreateService());
            service.Connect(null, null, null, null);
            ITaskFolder root = scope.Track(service.GetFolder("\\"));
            root.DeleteTask(_taskName, 0);
        }
        catch (COMException exception) when (IsTaskNotFound(exception))
        {
            // 已不存在等同于删除成功。
        }
        catch (DirectoryNotFoundException)
        {
            // 已不存在等同于删除成功。
        }
        catch (FileNotFoundException)
        {
            // 已不存在等同于删除成功。
        }
        finally
        {
            scope.Dispose();
        }
    }

    private string BuildTaskXml(
        string userName,
        string userSid,
        string workingDirectory)
    {
        string author = EscapeXml(Environment.UserName);
        string escapedUserName = EscapeXml(userName);
        string escapedUserSid = EscapeXml(userSid);
        string escapedTaskName = EscapeXml(_taskName);
        string escapedExecutablePath = EscapeXml(_executablePath);
        string escapedWorkingDirectory = EscapeXml(workingDirectory);

        return
            $"""
            <?xml version="1.0" encoding="UTF-16"?>
            <Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
              <RegistrationInfo>
                <Author>{author}</Author>
                <Description>校园网自动登录程序（当前用户）</Description>
                <URI>\{escapedTaskName}</URI>
              </RegistrationInfo>
              <Principals>
                <Principal id="Author">
                  <UserId>{escapedUserSid}</UserId>
                  <LogonType>InteractiveToken</LogonType>
                </Principal>
              </Principals>
              <Settings>
                <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
                <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
                <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
                <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
                <StartWhenAvailable>true</StartWhenAvailable>
              </Settings>
              <Triggers>
                <LogonTrigger>
                  <Delay>PT10S</Delay>
                  <UserId>{escapedUserName}</UserId>
                </LogonTrigger>
              </Triggers>
              <Actions Context="Author">
                <Exec>
                  <Command>{escapedExecutablePath}</Command>
                  <Arguments>--autostart</Arguments>
                  <WorkingDirectory>{escapedWorkingDirectory}</WorkingDirectory>
                </Exec>
              </Actions>
            </Task>
            """;
    }

    private static ITaskService CreateService()
    {
        Guid classId = new("0F87369F-A4E5-4CFC-BD3E-73E6154572DD");
        Guid interfaceId = typeof(ITaskService).GUID;
        int result = CoCreateInstance(
            ref classId,
            IntPtr.Zero,
            classContext: 5,
            ref interfaceId,
            out ITaskService service);
        Marshal.ThrowExceptionForHR(result);
        return service;
    }

    private static string EscapeXml(string value) =>
        SecurityElement.Escape(value) ?? string.Empty;

    private static bool PathsEqual(string left, string right)
    {
        try
        {
            return string.Equals(
                Path.GetFullPath(left).TrimEnd(Path.DirectorySeparatorChar),
                Path.GetFullPath(right).TrimEnd(Path.DirectorySeparatorChar),
                StringComparison.OrdinalIgnoreCase);
        }
        catch (Exception)
        {
            return false;
        }
    }

    private static bool IsTaskNotFound(COMException exception) =>
        exception.HResult is
            unchecked((int)0x80070002) or
            unchecked((int)0x80070003) or
            unchecked((int)0x8004130F);

    [DllImport("ole32.dll", ExactSpelling = true, PreserveSig = true)]
    private static extern int CoCreateInstance(
        ref Guid classId,
        IntPtr outer,
        uint classContext,
        ref Guid interfaceId,
        [MarshalAs(UnmanagedType.Interface)] out ITaskService service);

    [ComImport]
    [Guid("2FABA4C7-4DA9-4013-9697-20CC3FD40F85")]
    [InterfaceType(ComInterfaceType.InterfaceIsDual)]
    private interface ITaskService
    {
        ITaskFolder GetFolder([MarshalAs(UnmanagedType.BStr)] string path);

        [return: MarshalAs(UnmanagedType.Interface)]
        object GetRunningTasks(int flags);

        [return: MarshalAs(UnmanagedType.Interface)]
        object NewTask(uint flags);

        void Connect(
            [MarshalAs(UnmanagedType.Struct)] object? server,
            [MarshalAs(UnmanagedType.Struct)] object? user,
            [MarshalAs(UnmanagedType.Struct)] object? domain,
            [MarshalAs(UnmanagedType.Struct)] object? password);

        [return: MarshalAs(UnmanagedType.VariantBool)]
        bool GetConnected();

        [return: MarshalAs(UnmanagedType.BStr)]
        string GetTargetServer();

        [return: MarshalAs(UnmanagedType.BStr)]
        string GetConnectedUser();

        [return: MarshalAs(UnmanagedType.BStr)]
        string GetConnectedDomain();

        uint GetHighestVersion();
    }

    [ComImport]
    [Guid("8CFAC062-A080-4C15-9A88-AA7C2AF80DFC")]
    [InterfaceType(ComInterfaceType.InterfaceIsDual)]
    private interface ITaskFolder
    {
        [return: MarshalAs(UnmanagedType.BStr)]
        string GetName();

        [return: MarshalAs(UnmanagedType.BStr)]
        string GetPath();

        ITaskFolder GetFolder([MarshalAs(UnmanagedType.BStr)] string path);

        [return: MarshalAs(UnmanagedType.Interface)]
        object GetFolders(int flags);

        ITaskFolder CreateFolder(
            [MarshalAs(UnmanagedType.BStr)] string name,
            [MarshalAs(UnmanagedType.Struct)] object? securityDescriptor);

        void DeleteFolder(
            [MarshalAs(UnmanagedType.BStr)] string name,
            int flags);

        IRegisteredTask GetTask(
            [MarshalAs(UnmanagedType.BStr)] string path);

        [return: MarshalAs(UnmanagedType.Interface)]
        object GetTasks(int flags);

        void DeleteTask(
            [MarshalAs(UnmanagedType.BStr)] string name,
            int flags);

        IRegisteredTask RegisterTask(
            [MarshalAs(UnmanagedType.BStr)] string path,
            [MarshalAs(UnmanagedType.BStr)] string xml,
            int flags,
            [MarshalAs(UnmanagedType.Struct)] object? user,
            [MarshalAs(UnmanagedType.Struct)] object? password,
            int logonType,
            [MarshalAs(UnmanagedType.Struct)] object? securityDescriptor);
    }

    [ComImport]
    [Guid("9C86F320-DEE3-4DD1-B972-A303F26B061E")]
    [InterfaceType(ComInterfaceType.InterfaceIsDual)]
    private interface IRegisteredTask
    {
        [return: MarshalAs(UnmanagedType.BStr)]
        string GetName();

        [return: MarshalAs(UnmanagedType.BStr)]
        string GetPath();

        int GetState();

        [return: MarshalAs(UnmanagedType.VariantBool)]
        bool GetEnabled();

        void SetEnabled([MarshalAs(UnmanagedType.VariantBool)] bool enabled);

        [return: MarshalAs(UnmanagedType.Interface)]
        object Run([MarshalAs(UnmanagedType.Struct)] object? parameters);

        [return: MarshalAs(UnmanagedType.Interface)]
        object RunEx(
            [MarshalAs(UnmanagedType.Struct)] object? parameters,
            int flags,
            int sessionId,
            [MarshalAs(UnmanagedType.BStr)] string user);

        [return: MarshalAs(UnmanagedType.Interface)]
        object GetInstances(int flags);

        DateTime GetLastRunTime();

        int GetLastTaskResult();

        int GetNumberOfMissedRuns();

        DateTime GetNextRunTime();

        [return: MarshalAs(UnmanagedType.Interface)]
        object GetDefinition();

        [return: MarshalAs(UnmanagedType.BStr)]
        string GetXml();
    }

    private sealed class ComScope : IDisposable
    {
        private readonly List<object> _objects = [];

        public T Track<T>(T value)
            where T : class
        {
            _objects.Add(value);
            return value;
        }

        public void Dispose()
        {
            for (int index = _objects.Count - 1; index >= 0; index--)
            {
                object value = _objects[index];
                if (Marshal.IsComObject(value))
                {
                    try
                    {
                        Marshal.FinalReleaseComObject(value);
                    }
                    catch (InvalidComObjectException)
                    {
                        // 已释放。
                    }
                }
            }
        }
    }
}
