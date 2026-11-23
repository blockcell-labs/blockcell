"""Upstream transport; native helper is optional and receives secrets on stdin."""
import json
import ipaddress
import re
import socket
import ssl
import subprocess
import urllib.error
import urllib.request
from urllib.parse import urlsplit


def resolve_addresses(host, port):
    try:
        values = [record[4][0] for record in socket.getaddrinfo(host, port, type=socket.SOCK_STREAM)]
    except socket.gaierror:
        if not re.fullmatch(r'[a-zA-Z0-9.-]+', host):
            raise ValueError('invalid DNS hostname')
        values = subprocess.check_output(['dig', '+short', '+time=2', '+tries=1', host],
                                         text=True, timeout=6).splitlines()
    addresses = []
    for value in values:
        try:
            address = ipaddress.ip_address(value)
        except ValueError:
            continue
        item = f'[{address}]:{port}' if address.version == 6 else f'{address}:{port}'
        if item not in addresses:
            addresses.append(item)
    if not addresses:
        raise OSError('upstream hostname has no usable addresses')
    return addresses


class NativeResponse:
    def __init__(self, process, metadata):
        self.process = process
        self.status = metadata['status']
        self.headers = metadata['headers']

    def read(self, *args):
        return self.process.stdout.read(*args)

    def __iter__(self):
        return iter(self.process.stdout)

    def __enter__(self):
        return self

    def close(self):
        self.process.stdout.close()
        if self.process.poll() is None:
            self.process.terminate()
        try:
            self.process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait()

    def __exit__(self, *args):
        self.close()


def open_upstream(url, body, key, user_agent, native_command=None):
    if not native_command:
        request = urllib.request.Request(url, data=body, headers={
            'Content-Type': 'application/json', 'Authorization': 'Bearer ' + key, 'User-Agent': user_agent})
        # Construct an explicit verified TLS context, independent of site hooks.
        return urllib.request.urlopen(request, timeout=120, context=ssl.create_default_context())
    process = subprocess.Popen(native_command, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                               stderr=subprocess.DEVNULL, start_new_session=True)
    try:
        parsed = urlsplit(url)
        addresses = resolve_addresses(parsed.hostname, parsed.port or (443 if parsed.scheme == 'https' else 80))
        process.stdin.write(json.dumps({'url': url, 'body': json.loads(body), 'key': key, 'user_agent': user_agent,
                                       'addresses': addresses}).encode())
        process.stdin.close()
        metadata = json.loads(process.stdout.readline())
        response = NativeResponse(process, metadata)
        if response.status >= 400:
            raise urllib.error.HTTPError(url, response.status, 'upstream request failed', response.headers, response)
        return response
    except urllib.error.HTTPError:
        raise
    except BaseException:
        process.kill()
        process.wait()
        process.stdout.close()
        raise OSError('native upstream transport failed') from None
