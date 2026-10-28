# Directory snapshot wire format

`directory-v1-long.tar` fixes the complete version-one archive encoding for:

- `package.json`, regular file, 0644, bytes `{}`;
- a directory whose name is 120 ASCII `x` characters, mode 0755;
- `file` inside that directory, regular file, 0644, bytes `long`.

Entries are in byte order. The fixture was assembled from the GNU header field
layout independently of the production `tar::Builder`: zero-filled 512-byte
headers, GNU magic `ustar ` and version ` \0`, zero uid/gid/mtime, NUL-terminated
zero-padded octal numeric fields, including seven octal checksum digits followed
by NUL. Checksums count their eight-byte field as spaces. Each long path is
preceded by a regular GNU `L` extension named `././@LongLink`, mode 0644, whose
payload is the full path followed by NUL. The following header contains its first
100 path bytes. Payload padding and the final two blocks are zero.

The archive is 5632 bytes, SHA-256
`c39389c4354aa219ad4c43f302794f6f4618b6f4fa07354978e30ee16ddb2180`.
Changing this fixture changes persisted directory identities and requires review
of the version-one source contract. Do not regenerate it from capture output.
