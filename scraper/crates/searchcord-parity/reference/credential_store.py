"""Collector credentials live in a sealed vault outside the SQLite archive."""
import ctypes
import hashlib
import json
import os
from pathlib import Path
import stat
import tempfile

_USE_DPAPI = os.name == 'nt'


class CredentialError(RuntimeError):
    pass


def _paths(database):
    archive = Path(database).resolve()
    archive_key = hashlib.sha256(os.path.normcase(str(archive)).encode()).hexdigest()[:24]
    root = Path(os.environ.get('LOCALAPPDATA') or
                os.environ.get('XDG_DATA_HOME') or Path.home() / '.local' / 'share')
    vault = Path(os.environ.get('SEARCHCORD_CREDENTIAL_FILE') or
                 root / 'Searchcord' / 'credentials' / f'{archive_key}.vault').expanduser().resolve()
    key = Path(os.environ.get('SEARCHCORD_CREDENTIAL_KEY_FILE') or
               vault.with_suffix('.key')).expanduser().resolve()
    if vault.is_relative_to(archive.parent) or key.is_relative_to(archive.parent):
        raise CredentialError('Keep the credential vault and encryption key outside the archive directory')
    if vault == key:
        raise CredentialError('The credential vault and encryption key must use different files')
    return vault, key


def _private(path):
    if os.name != 'nt' and stat.S_IMODE(path.stat().st_mode) & 0o077:
        raise CredentialError('Credential files must be readable only by their owner (chmod 600)')


def _directory(path):
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)


def _dpapi(data, decrypt=False):
    """Use the current Windows user's DPAPI; never request an interactive UI."""
    from ctypes import wintypes

    class Blob(ctypes.Structure):
        _fields_ = [('size', wintypes.DWORD), ('data', ctypes.POINTER(ctypes.c_ubyte))]

    crypt32 = ctypes.WinDLL('crypt32', use_last_error=True)
    kernel32 = ctypes.WinDLL('kernel32', use_last_error=True)
    kernel32.LocalFree.argtypes = [ctypes.c_void_p]
    kernel32.LocalFree.restype = ctypes.c_void_p
    buffer = ctypes.create_string_buffer(data)
    incoming = Blob(len(data), ctypes.cast(buffer, ctypes.POINTER(ctypes.c_ubyte)))
    outgoing = Blob()
    description = ctypes.c_wchar_p()
    if decrypt:
        operation = crypt32.CryptUnprotectData
        operation.argtypes = [ctypes.POINTER(Blob), ctypes.POINTER(ctypes.c_wchar_p),
                              ctypes.POINTER(Blob), ctypes.c_void_p, ctypes.c_void_p,
                              wintypes.DWORD, ctypes.POINTER(Blob)]
        ok = operation(ctypes.byref(incoming), ctypes.byref(description), None,
                       None, None, 1, ctypes.byref(outgoing))
    else:
        operation = crypt32.CryptProtectData
        operation.argtypes = [ctypes.POINTER(Blob), ctypes.c_wchar_p,
                              ctypes.POINTER(Blob), ctypes.c_void_p, ctypes.c_void_p,
                              wintypes.DWORD, ctypes.POINTER(Blob)]
        ok = operation(ctypes.byref(incoming), 'Searchcord credentials', None,
                       None, None, 1, ctypes.byref(outgoing))
    operation.restype = wintypes.BOOL
    if not ok:
        raise CredentialError('Windows could not unlock the credential vault for this user')
    try:
        return ctypes.string_at(outgoing.data, outgoing.size)
    finally:
        kernel32.LocalFree(outgoing.data)
        if description:
            kernel32.LocalFree(ctypes.cast(description, ctypes.c_void_p))


def _fernet(key_path, create=False):
    from cryptography.fernet import Fernet
    if not key_path.exists():
        if not create:
            raise CredentialError('The external credential encryption key is missing; restore it or clear credentials')
        _directory(key_path)
        try:
            descriptor = os.open(key_path, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
        except FileExistsError:
            pass
        else:
            with os.fdopen(descriptor, 'wb') as output:
                output.write(Fernet.generate_key())
                output.flush()
                os.fsync(output.fileno())
    _private(key_path)
    try:
        return Fernet(key_path.read_bytes().strip())
    except (ValueError, TypeError):
        raise CredentialError('The external credential encryption key is invalid') from None


def _validate(state):
    if not isinstance(state, dict) or not isinstance(state.get('tokens'), list):
        raise CredentialError('The credential vault is invalid')
    ids = set()
    for token in state['tokens']:
        if (not isinstance(token, dict) or any(not isinstance(token.get(key), str) or not token[key]
                                              for key in ('id', 'label', 'token'))
                or token['id'] in ids):
            raise CredentialError('The credential vault is invalid')
        ids.add(token['id'])
    if state.get('active_token_id') not in ids | {None}:
        raise CredentialError('The active credential is missing from the vault')
    return state


def _unseal(data, key):
    try:
        header, ciphertext = data.split(b'\n', 1)
        if header == b'SEARCHCORD-DPAPI-1' and _USE_DPAPI:
            plaintext = _dpapi(ciphertext, decrypt=True)
        elif header == b'SEARCHCORD-FERNET-1':
            plaintext = _fernet(key).decrypt(ciphertext)
        else:
            raise CredentialError('This credential vault needs its original OS user or encryption key')
        return _validate(json.loads(plaintext))
    except CredentialError:
        raise
    except Exception:
        raise CredentialError('Could not unlock the credential vault; saved credentials were kept') from None


def load_credentials(database):
    vault, key = _paths(database)
    if not vault.exists():
        return {'tokens': [], 'active_token_id': None}
    _private(vault)
    try:
        return _unseal(vault.read_bytes(), key)
    except OSError:
        raise CredentialError('Could not read the credential vault; saved credentials were kept') from None


def save_credentials(database, tokens, active_token_id):
    vault, key = _paths(database)
    state = _validate({'tokens': tokens, 'active_token_id': active_token_id})
    # A missing key must never silently replace a vault containing older tokens.
    if vault.exists():
        load_credentials(database)
    plaintext = json.dumps(state, ensure_ascii=False, separators=(',', ':')).encode('utf-8')
    if _USE_DPAPI:
        data = b'SEARCHCORD-DPAPI-1\n' + _dpapi(plaintext)
    else:
        data = b'SEARCHCORD-FERNET-1\n' + _fernet(key, create=not vault.exists()).encrypt(plaintext)
    _directory(vault)
    temporary = None
    try:
        with tempfile.NamedTemporaryFile(dir=vault.parent, prefix='.credential-', delete=False) as output:
            temporary = Path(output.name)
            output.write(data)
            output.flush()
            os.fsync(output.fileno())
        # Verify the sealed bytes before replacing an older working vault.
        if _unseal(temporary.read_bytes(), key) != state:
            raise CredentialError('The credential vault did not verify; archive settings were kept')
        os.replace(temporary, vault)
        temporary = None
        if os.name != 'nt':
            descriptor = os.open(vault.parent, os.O_RDONLY)
            try:
                os.fsync(descriptor)
            finally:
                os.close(descriptor)
    finally:
        if temporary:
            temporary.unlink(missing_ok=True)


def clear_credentials(database):
    vault, _ = _paths(database)
    # The separate key contains no tokens and may be retained for the next vault.
    vault.unlink(missing_ok=True)


def credential_metadata(tokens):
    return [{'id': token['id'], 'label': token['label']} for token in tokens]


def migrate_credentials(database, db):
    """Seal legacy settings before any new database recovery copy is created."""
    if not db.execute("SELECT 1 FROM sqlite_master WHERE name='settings' AND type='table'").fetchone():
        return False
    settings = dict(db.execute("SELECT key,value FROM settings WHERE key IN ('token','saved_tokens','active_token_id')"))
    raw = settings.get('saved_tokens')
    try:
        tokens = json.loads(raw) if raw is not None else []
    except (ValueError, TypeError):
        raise CredentialError('Legacy credential settings are invalid; archive was kept') from None
    if not isinstance(tokens, list):
        raise CredentialError('Legacy credential settings are invalid; archive was kept')
    legacy = settings.get('token')
    has_plaintext = bool(legacy) or any(isinstance(item, dict) and 'token' in item for item in tokens)
    if not has_plaintext:
        return False
    if legacy and not tokens:
        tokens = [{'id': 'legacy', 'label': 'Saved token', 'token': legacy}]
    active = settings.get('active_token_id') or (tokens[0]['id'] if tokens else None)
    _validate({'tokens': tokens, 'active_token_id': active})
    state = load_credentials(database)
    merged = {token['id']: token for token in state['tokens']}
    for token in tokens:
        if token['id'] in merged and merged[token['id']]['token'] != token['token']:
            raise CredentialError('Legacy credential IDs conflict with the vault; settings were kept')
        merged[token['id']] = token
    tokens = list(merged.values())
    save_credentials(database, tokens, state['active_token_id'] or active)
    # Logical removal prevents new plaintext writes. Old DB/WAL pages and old
    # backups still need the owner's private handling and credential rotation.
    db.execute('BEGIN IMMEDIATE')
    try:
        db.execute("DELETE FROM settings WHERE key='token'")
        db.executemany('INSERT OR REPLACE INTO settings(key,value) VALUES (?,?)',
                       [('saved_tokens', json.dumps(credential_metadata(tokens))),
                        ('active_token_id', state['active_token_id'] or active)])
        db.commit()
    except BaseException:
        db.rollback()
        raise
    return True
