# Read-only SFTP access to product data

Production data lives under the repository/deployment `var/` directory. A
dedicated `sftpdata` account can expose this tree read-only without placing SSH
configuration or credentials in Git.

The recommended layout is:

```text
host filesystem                 sftpdata chroot
<deployment>/var        bind -> /var/sftp-ro/var
                                  ^ login starts here as /var
```

`/var/sftp-ro` must be owned by `root:root` and must not be group-writable.
The mounted `var` directory may remain owned by the service account because
`internal-sftp -R` enforces read-only access for `sftpdata`.

Deployment outline:

1. Create the unprivileged account with `/usr/sbin/nologin` and home `/var`.
2. Create root-owned `/var/sftp-ro` and its `var` mount point.
3. Copy and adapt `fstab.bind.example` into `/etc/fstab`, then mount it.
4. Copy `99-sftpdata.conf.example` into `/etc/ssh/sshd_config.d/`.
5. Validate with `sudo sshd -t`, then reload SSH.
6. Verify the effective match with
   `sudo sshd -T -C user=sftpdata,host=localhost,addr=127.0.0.1`.

Do not expose the former `data/` directory. It is a retired layout; recordings,
archives, logs and diagnostics are all rooted under `var/`.
