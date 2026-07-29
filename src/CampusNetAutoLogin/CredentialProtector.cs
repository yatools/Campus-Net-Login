using System.Security.Cryptography;
using System.Text;

namespace CampusNetAutoLogin;

public interface ICredentialProtector
{
    string Encrypt(string plaintext);
    bool TryDecrypt(string encryptedBase64, out string plaintext);
}

public sealed class CredentialProtector : ICredentialProtector
{
    private static readonly byte[] Entropy =
        "CampusNetAutoLogin/v1/CurrentUser"u8.ToArray();

    public string Encrypt(string plaintext)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(plaintext);

        byte[] plaintextBytes = Encoding.UTF8.GetBytes(plaintext);
        byte[]? encryptedBytes = null;
        try
        {
            encryptedBytes = ProtectedData.Protect(
                plaintextBytes,
                Entropy,
                DataProtectionScope.CurrentUser);
            return Convert.ToBase64String(encryptedBytes);
        }
        finally
        {
            CryptographicOperations.ZeroMemory(plaintextBytes);
            if (encryptedBytes is not null)
            {
                CryptographicOperations.ZeroMemory(encryptedBytes);
            }
        }
    }

    public bool TryDecrypt(string encryptedBase64, out string plaintext)
    {
        plaintext = string.Empty;
        if (string.IsNullOrWhiteSpace(encryptedBase64))
        {
            return false;
        }

        byte[]? encryptedBytes = null;
        byte[]? plaintextBytes = null;
        try
        {
            encryptedBytes = Convert.FromBase64String(encryptedBase64);
            plaintextBytes = ProtectedData.Unprotect(
                encryptedBytes,
                Entropy,
                DataProtectionScope.CurrentUser);
            plaintext = Encoding.UTF8.GetString(plaintextBytes);
            return true;
        }
        catch (CryptographicException)
        {
            return false;
        }
        catch (FormatException)
        {
            return false;
        }
        finally
        {
            if (encryptedBytes is not null)
            {
                CryptographicOperations.ZeroMemory(encryptedBytes);
            }

            if (plaintextBytes is not null)
            {
                CryptographicOperations.ZeroMemory(plaintextBytes);
            }
        }
    }
}
