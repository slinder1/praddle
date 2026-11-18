# Praddle test server

This crate implements the GitHub behavior used by Praddle. It serves:

* GitHub GraphQL and REST requests over a Unix socket for the real `gh` CLI.
* A bare repository for direct file-based fetches and atomic pushes by the real `git` CLI.
* A `pre-receive` hook that rejects branch deletions and non-fast-forward updates.
* A `post-receive` hook that reports pushes over the Unix socket.
* `GET /_test/state` for test assertions.

After each push, the server marks every open pull request as merged when its
head ref is reachable from its base ref, matching GitHub's behavior.

Run it with a single repository identity and an existing bare Git repository:

```console
cargo run -p praddle-test-server -- OWNER REPO /path/to/repo.git --socket /tmp/praddle-github.sock
```

The first stdout line contains the Unix socket as JSON. Configure an isolated
`gh` config with `http_unix_socket` set to the socket. Redirect Git to the bare
repository without changing the GitHub repository URL with:

```console
git config --global url.file:///path/to/repo.git.insteadOf https://github.com/OWNER/REPO
```

Tests should prefer `TestHarness::start`. It creates a temporary bare remote and
worktree, configures a test identity, commits and pushes an empty `main` base,
checks out a `change` branch, starts the server, and isolates both `gh` and Git
settings from the user's configuration. `TestHarness::git`,
`TestHarness::write`, and `TestHarness::command` provide the common operations
needed by scenarios.

`TestRepository` and `TestServer` remain available separately for tests that
need nonstandard repository or server setup.
