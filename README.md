GitHub stacked-PR builder for those who miss Gerrit

See `--help` for details.

# Oversized Comments

Comments exceeding GitHub's 65,536-byte UTF-8 limit are not posted. Praddle
instead saves the complete Markdown to
`$XDG_STATE_HOME/praddle/comments` (default: `~/.local/state/praddle/comments`)
and prints a warning and the saved file's path. Filenames include the repository
owner, repository name, PR number, and a unique suffix so earlier comments are
not overwritten. The push continues normally if the comment is saved. Dry runs
only warn and do not save files.

# Etymology

<sub>(*Note:* This project was previously named `gd` and then `cgh`)</sub>

The name doesn't meaning anything, but it sounds like "prattle", which I enjoy.

A backronym might be "Pull Request, Add (Little Endian)".

# LLM use

It is unfortunate that this tool needs to exist at all, and even more
unfortunate that I don't have the time to dedicate to doing it right.

All development up to commit 0948335884fcb2645ada3985ff4c05dcf075b4f2 (tag
v0.10.0) was done by hand without LLMs, but at the point when GitHub rolled out
their (rather poor) support for stacks as a reified concept, I started using
LLMs to deal with what I perceived as design defects in their model that I
don't have the time or energy to slog through papering over, but which I have
to interact with day-to-day.

Ultimately I was pointed to https://github.com/ezyang/ghstack which showed me I
was wrong about how GH renders merge-based ff-only stacks, and so I ended up
using LLMs to "port" praddle to the approach used in ghstack. Many thanks to
the devs of that tool. I would just be using ghstack if not for a lot of other
aesthetic and workflow issues I have with its design, at least for my own
personal use.

Ultimately I would consider `praddle` to be a vibecoded hack now, although
I have at least looked at all of the code and nudged it towards being
readable.

# Testing

The `praddle-test-server` workspace crate provides a minimal, stateful GitHub
implementation for integration tests, originally based on the approach
in [ghstack's github_fake](https://github.com/ezyang/ghstack/blob/f4e9df551eda204bc95b4329870ea304bd20fbb0/src/ghstack/github_fake.py). It serves the GraphQL and REST operations
used by praddle over a Unix socket and redirects Git operations directly to a
local bare repository. This lets tests invoke the real `gh`, `git`, and
`praddle` executables without accessing GitHub or changing the user's
configuration.

Run the suite with `cargo test --workspace`. See `test-server/README.md` for
standalone server usage.

# Alternatives

## `ghstack`

If you're here, you probably can just use `ghstack` instead of praddle. My
beefs with `ghstack` are only:

* That it seems kind of Meta-centric, with some hard-coded Meta-isms in the
  codebase.
* It also has support for interacting with CI and a few other things that I
  don't need a tool for stacking PRs to touch.
* It will modify commits for you, outside of a Gerrit-like commit-msg hook.
* It uses a lot of redundant commit message trailers. It seems like it does
  this as sort of a cache to avoid some GH API queries sometimes (?) unclear,
  but I don't like them.

Otherwise, `ghstack` seems to do everything right, and in a principled way. It
is probably the better choice if you don't mind the above caveats.

## `spr`

Somewhat confusingly there are at least two tools which nominally do the same
thing and are called `spr`. I'm unsure which came first, and it doesn't really
matter, but it is unfortunate they collide.

### `ejoffe/spr`

A very compelling alternative to `praddle` is https://github.com/ejoffe/spr which
differs in a few ways:

* `spr` will modify your local branches by default for logically
  non-destructive operations (i.e. when you try to `update` the remote)
* `spr` won't use Gerrit `Change-Id:`, and is very particular about the format
  of its ID; `praddle` allows any string and uses the `Change-Id:` trailer
* `spr` does not seem to have a `dry-run` option, which makes using it
  generally more nerve-wracking than necessary
* `spr` doesn't produce an "interdiff" when force-pushing to give the reviewer
  context for the edits to the change
* `spr` installs itself as a git subcommand (this is really just an aesthetic
  quibble, but I don't think it is primarily a `git` tool, it is a GitHub tool,
  and exists only to patch a deficiency in GitHub as a service)
* `spr` warns you to only close/merge PRs through it, rather than just
  diagnosing when e.g. a PR would be created for a change which already has a
  merged PR
* `spr` uses YAML for configuration, `praddle` uses TOML
* `spr` is noisy by default, `praddle` is quiet by default
* `spr` seems slightly less aggressive with parallelizing operations
* `spr` is written in Go, `praddle` is written in Rust

In the end most of these are fairly aesthetic and minor, but rather than try to
hack on `spr` I opted to start over and make the exact tool I wanted. YMMV

### `spacedentist/spr`

Another great contender, https://github.com/spacedentist/spr uses multiple
branches per PR to allow the local branch to be maintained as a set of changes
that is amended and rebased, while only fast-forwarding remote refs associated
with PRs so the GitHub UI doesn't throw away the context of comments and
collapse timeline entries.

The biggest issue (now) with this approach is that GitHub chose to codify the
force-push as a requirement in their model of stacks. So now, you can either
choose to contort your remote refs to avoid GitHub throwing away a bunch of
valuable information when you force-push, or you can have the new shiny
stack UI and merge queue. You can't have both!

There are other quibbles I have with this `spr`, but I think this is now the
overriding reason that I will not adopt this model.

## `gherrit`

A tool with a very similar core philosophy, https://github.com/joshlf/gherrit
seems to differ primarily in the UX and the structure of remote branches:

* Goes to greater lengths to reproduce the `git push`-based workflow of Gerrit
  proper. This involves intercepting the `push` through hooks.
* Retains more "phantom branches" on the remote to facilitate diffs and retain
  comments (if I understand it correctly).
* Also includes GH actions to keep the stack tidy and ready to merge. I don't
  fully understand how this works yet.

## `maiao`

I haven't actually used https://github.com/adevinta/maiao but came across it
since writing `praddle`. The biggest issue I see immediately is that it modifies
local refs to do fixups and rebases.

## `graphite`

I have had only negative experiences with https://graphite.dev/

In particular my issues are:

* Modifies local refs, and inserts itself into your workflow before you even
  consider creating PRs
* Requires a third-party service
* Is terribly slow (on top of the already slow GH API)
* Is closed-source
