# Security policy

## Supported versions

Security fixes target the latest versioned release and the current `main`
branch. Older pre-1.0 releases do not receive a guaranteed backport; affected
versions and upgrade guidance will be stated with each coordinated fix.

## Reporting a vulnerability

Please use GitHub's private vulnerability reporting for this repository from the
**Security** tab when it is available. Include a concise impact statement,
reproduction steps, affected commit or version, and any proposed mitigation.

If private reporting is not available, open a public issue containing no exploit
details or sensitive data and ask the maintainer to establish a private contact
channel. Do not attach proof-of-concept code, private logs, wallpaper files, or
unredacted local paths to that issue.

Relevant security boundaries include:

- the same-user public Unix socket versus the inherited private renderer channel;
- image decoding and cache generation from local files;
- renderer supervision and diagnostic collection;
- future shader/effect loading;
- file moves performed by quarantine actions.

Reports will be acknowledged as maintainer availability permits. Details should
remain private until a fix and coordinated disclosure plan are ready.
