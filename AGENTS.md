# Git policy

- Use commit messages that are concise, descriptive, and complete enough for future history readers: include each material behavior change or fix and why it matters, but avoid filler, repetition, or mechanically restating the diff.
- Prefer a specific subject plus a short body for non-trivial or multi-part commits. The subject should name the main theme, and the body should call out distinct changes when omitting one would make the history misleading.
- Avoid vague subjects like "update", "fix", or "refactor".

# Diagnostics

- When memory leaks are plausible, use the installed `valgrind` tooling to check leak behavior while working. Prefer targeted runs around the suspected command or daemon path because Valgrind is slow.

# Publication Boundary

- Treat every file, commit, branch, tag, and reachable history as potentially
  public, even while the remote repository is private.
- Do not commit secrets, private network details, personal data, private issue
  links, environment identifiers, or unnecessary absolute local paths.
- Keep documentation, examples, fixtures, and defaults portable for external
  users and contributors.
- Preserve unrelated dirty work and stage exact paths.
- Before exposing previously private history, scan both the current tree and
  reachable Git history for private or environment-specific material.
- Validate, commit, and push completed task-owned changes to the intended
  existing remote without a separate push request. Push directly when permitted;
  if main or the target branch is protected, push a task branch and follow the
  required pull-request/review/check workflow without bypassing protections.
- Review the complete outgoing commit range, exclude unrelated work and secrets,
  avoid force-pushes, and verify the remote branch after pushing.
- New remotes, visibility changes, release/tag publication, and deployment
  require separate explicit authorization. A push that itself releases or
  deploys also needs that separate authorization.
