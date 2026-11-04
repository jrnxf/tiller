//! The classifier and fragment tests of `authority.md` section 12: every
//! deny and allow decision of sections 8 to 10, fed as the hook payload, for
//! every role. Allow cases matter as much as denies: a false block costs
//! output (hard rule 9).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::*;
use crate::roles::{Placeholders, render_settings};

const HOME: &str = "/home/user";
const WORKTREE: &str = "/home/user/pool/3/alpha";
const TILLER: &str = "/nix/store/abc-tiller-0.1.0/bin/tiller";
const BRANCH: &str = "fm/alpha-1";

fn env() -> Env {
    Env {
        tiller_home: PathBuf::from("/home/user/.tiller"),
        user_home: Some(PathBuf::from(HOME)),
        tiller_session_id: None,
        tiller_run_id: None,
        packaged_tiller: Some(PathBuf::from(TILLER)),
        protected_roots: vec![
            PathBuf::from("/home/user/checkouts"),
            PathBuf::from("/home/user/pool"),
        ],
    }
}

fn facts() -> RunFacts {
    RunFacts {
        branch: Some(BRANCH.into()),
        default_branch: Some("main".into()),
    }
}

fn cwd_for(role: Role) -> PathBuf {
    match role {
        Role::Orchestrator => PathBuf::from("/home/user/tiller-homes/alpha"),
        _ => PathBuf::from(WORKTREE),
    }
}

fn tool_in(role: Role, cwd: &Path, tool: &str, input: Value) -> Option<Denial> {
    let env = env();
    let load = facts;
    let ctx = Context::new(role, &env, cwd.to_path_buf(), &load);
    decide(tool, &input, &ctx)
}

fn tool(role: Role, tool: &str, input: Value) -> Option<Denial> {
    tool_in(role, &cwd_for(role), tool, input)
}

fn bash_in(role: Role, cwd: &Path, command: &str) -> Option<Denial> {
    tool_in(role, cwd, "Bash", json!({ "command": command }))
}

fn bash(role: Role, command: &str) -> Option<Denial> {
    bash_in(role, &cwd_for(role), command)
}

#[track_caller]
fn denied(role: Role, command: &str, code: Code) {
    match bash(role, command) {
        Some(denial) => assert_eq!(denial.code, code, "{role}: {command}: {}", denial.message()),
        None => panic!("{role}: {command}: allowed, wanted [{code}]"),
    }
}

#[track_caller]
fn allowed(role: Role, command: &str) {
    if let Some(denial) = bash(role, command) {
        panic!("{role}: {command}: denied {}", denial.message());
    }
}

#[track_caller]
fn denied_for_all(command: &str, code: Code) {
    for role in Role::ALL {
        denied(role, command, code);
    }
}

/// A throwaway repository whose HEAD is on `branch`, as a linked worktree
/// with a `.git` file, the shape treehouse creates.
fn repo_on(branch: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let gitdir = dir.path().join("gitdir");
    fs::create_dir_all(&gitdir).unwrap();
    fs::write(gitdir.join("HEAD"), format!("ref: refs/heads/{branch}\n")).unwrap();
    fs::write(dir.path().join(".git"), "gitdir: gitdir\n").unwrap();
    dir
}

// -- G08 merge-capable GitHub commands ----------------------------------------

#[test]
fn merge_commands_denied_for_every_fleet_role() {
    for command in [
        "gh pr merge 12 --squash",
        "gh-axi pr merge 12",
        "gh pr merge --auto 12",
        "gh -R example/alpha pr merge 12",
        "gh-axi pr merge 12 -R example/alpha",
        "gh-axi stack merge 42 --squash",
        "gh pr close 12 --delete-branch",
        "gh auth token",
        "gh auth login --with-token",
        "gh-axi auth refresh",
        "gh repo delete example/alpha --yes",
        "gh mymerge 12",
    ] {
        denied_for_all(command, Code::Merge);
    }
    denied_for_all("gh repo edit --default-branch release", Code::DefaultPush);
    for command in [
        "gh-axi pr view 12",
        "gh-axi pr list --state open",
        "gh-axi pr checks 12",
        "gh auth status",
        "gh pr create --title 'merge the parser' --body 'gh pr merge is not run'",
        "gh-axi pr create --title merge --body x",
        "gh-axi issue list -R example/alpha",
    ] {
        for role in Role::ALL {
            allowed(role, command);
        }
    }
}

#[test]
fn raw_api_writes_denied() {
    for command in [
        "gh api -X PUT repos/o/r/pulls/12/merge",
        "gh api --method=PUT repos/o/r/pulls/12/merge",
        "gh api -XPOST repos/o/r/git/refs -f ref=refs/heads/main -f sha=abc",
        "gh api repos/o/r/git/refs -f ref=refs/heads/x -f sha=abc",
        "gh api repos/o/r/pulls/12/merge --input body.json",
        "gh api graphql -f query='mutation { mergePullRequest(input: {}) { clientMutationId } }'",
        "gh api graphql -F query=@merge.graphql",
        "gh-axi api -X PATCH repos/o/r -f default_branch=x",
        "gh api -X \"$METHOD\" repos/o/r/pulls/12/merge",
        // Attached field forms imply a POST just as the spaced ones do.
        "gh api repos/o/r/git/refs -fref=refs/heads/x -fsha=abc",
        "gh api repos/o/r/git/refs -Fref=refs/heads/x",
        "gh api repos/o/r/git/refs --field=ref=refs/heads/x",
        "gh api repos/o/r/git/refs --raw-field=ref=refs/heads/x",
        "gh api graphql -fquery='mutation { mergePullRequest(input: {}) { clientMutationId } }'",
    ] {
        denied_for_all(command, Code::Merge);
    }
    for role in Role::ALL {
        // Reads pass the backstop; the canonical deny rule still covers
        // `gh api *` in the permission layer.
        allowed(role, "gh api repos/o/r/pulls/12");
        allowed(role, "gh api graphql -f query='query { viewer { login } }'");
    }
}

#[test]
fn mcp_write_tools_denied() {
    for role in Role::ALL {
        for name in [
            "mcp__github__merge_pull_request",
            "mcp__github__push_files",
            "mcp__github__delete_branch",
            "mcp__github__create_ref",
            "mcp__forge__update_ref",
            // Commits straight to any branch, the default one included.
            "mcp__github__create_or_update_file",
        ] {
            let denial = tool(role, name, json!({})).expect(name);
            assert_eq!(denial.code, Code::Merge, "{role}: {name}");
        }
        assert_eq!(tool(role, "mcp__github__get_pull_request", json!({})), None);
    }
}

#[test]
fn wrapped_merge_forms_denied() {
    for command in [
        "sh -c 'gh pr merge 12'",
        "bash -lc \"gh pr merge 12\"",
        "bash -euo pipefail -c 'gh pr merge 12'",
        "/usr/bin/env gh pr merge 12",
        "env GH_REPO=o/r gh pr merge 12",
        "$(which gh) pr merge 12",
        "`command -v gh` pr merge 12",
        "$GH pr merge 12",
        "command gh pr merge 12",
        "/usr/local/bin/gh pr merge 12",
        "timeout 30 nice -n 5 nohup gh pr merge 12",
        "sudo -u root gh pr merge 12",
        "echo 12 | xargs gh pr merge",
        // xargs input that names the command itself.
        "echo pr merge 12 | xargs gh",
        "echo merge 12 | xargs gh pr",
        "echo merge | xargs -I{} gh pr {} 12",
        "echo merge | xargs --replace=@ gh pr @ 12",
        "echo -XPUT repos/o/r/pulls/12/merge | xargs gh api",
        "echo '12 -d' | xargs gh pr close",
        "echo 'gh pr merge 12' | xargs sh -c",
        "echo gh pr merge 12 | sh",
        "echo gh pr merge 12 | bash -s",
        "eval 'gh pr merge 12'",
        "eval \"$(printf 'gh pr merge 12')\"",
        "true && { gh pr merge 12; }",
        "if true; then gh pr merge 12; fi",
        "for n in 12; do gh pr merge $n; done",
        "x=$(gh pr merge 12)",
        "echo \"$(gh pr merge 12)\"",
        "cat <(gh pr merge 12)",
        "$'\\x67h' pr merge 12",
        "g''h pr m\"erg\"e 12",
        "GH=gh; $GH pr merge 12",
        "env -S 'gh pr merge 12'",
        "nix develop -c gh pr merge 12",
        "watch -n 5 gh pr merge 12",
        "find . -maxdepth 0 -exec gh pr merge 12 \\;",
        "bash <<'EOF'\ngh pr merge 12\nEOF",
        "bash <<< 'gh pr merge 12'",
        "echo 'gh pr merge 12' | bash",
        "f() { gh pr merge \"$1\"; }; f 12",
        "alias m='gh pr merge'; m 12",
        "su -c 'gh pr merge 12'",
        "flock /tmp/l -c 'gh pr merge 12'",
    ] {
        denied(Role::Builder, command, Code::Merge);
        denied(Role::Orchestrator, command, Code::Merge);
    }
    // Unreadable with a protected token: fail closed.
    denied(Role::Builder, "gh pr merge 'unclosed", Code::Merge);
    // Unreadable without one: allowed fast.
    allowed(Role::Builder, "echo 'unclosed");
}

// -- G09 HTTP clients ---------------------------------------------------------

#[test]
fn http_clients_denied() {
    for command in [
        "curl -X PUT -H \"Authorization: token $(gh auth token)\" https://api.github.com/repos/o/r/pulls/12/merge",
        "/usr/bin/curl https://example.com",
        "command curl https://example.com",
        "wget -qO- https://example.com",
        "http POST example.com",
        "xh example.com",
        "aria2c https://example.com/f",
        "nc example.com 443",
        "curl -X POST http://127.0.0.1:7421/api/v1/approvals/01J/decide",
        "socat - TCP:example.com:443",
        "openssl s_client -connect example.com:443",
        "env curl example.com",
    ] {
        let denial = bash(Role::Builder, command).expect(command);
        // The token form also holds `gh auth`, a merge code; either refuses it.
        assert!(
            matches!(denial.code, Code::HttpClient | Code::Merge),
            "{command}: {}",
            denial.message()
        );
    }
    denied_for_all("curl https://example.com", Code::HttpClient);
    allowed(Role::Builder, "grep -rn curl src/");
    allowed(Role::Builder, "echo 'use curl https://example.com'");
}

// -- G11 and G12 pushes -------------------------------------------------------

#[test]
fn default_branch_push_denied() {
    for command in [
        "git push origin HEAD:main",
        "git push origin main",
        "git push origin fm/alpha-1:main",
        "git push origin HEAD:refs/heads/main",
        "git -C . push origin HEAD:refs/heads/main",
        "git 'push' origin main",
        "g\\it push origin main",
        "git -c push.default=current push",
        "git -c remote.origin.push=refs/heads/*:refs/heads/main push",
        "/usr/bin/git push --mirror",
        "git push --all origin",
        "git push origin :fm/alpha-1",
        "git push --delete origin fm/alpha-1",
        "git push -d origin fm/alpha-1",
        "git push origin 'refs/heads/*:refs/heads/*'",
        "git send-pack origin HEAD:main",
        "git subtree push --prefix=x origin main",
        "cd sub && git push origin main",
        "sh -c 'git push origin main'",
        "echo main | xargs git push origin",
        "echo git push origin main | sh",
        "git --git-dir=.git --work-tree=. push origin main",
        // Substitutions inside parameter and arithmetic expansions run too.
        ": ${x:=$(git push origin HEAD:main)}",
        "echo \"${x:-`git push origin HEAD:main`}\"",
        "echo $(( $(git push origin HEAD:main) + 1 ))",
        "(( $(git push origin HEAD:main) ))",
    ] {
        denied(Role::Builder, command, Code::DefaultPush);
    }
    // xargs input that names the git subcommand.
    for command in [
        "echo push origin main | xargs git",
        "echo push | xargs -I{} git {} origin main",
        "echo push | xargs -I {} git {} origin main",
        "echo push | xargs -i git {} origin main",
    ] {
        denied(Role::Builder, command, Code::GuardTamper);
    }
    for command in [
        "git ls-files | xargs rg -n push",
        "git ls-files -z | xargs -0 -n 20 cat",
        "echo 12 | xargs gh pr view",
        "echo main | xargs -I{} git log {}",
        "echo ls | sh",
    ] {
        allowed(Role::Builder, command);
    }
}

#[test]
fn force_push_denied() {
    for command in [
        "git push --force origin fm/alpha-1",
        "git push -f origin fm/alpha-1",
        "git push -uf origin fm/alpha-1",
        "git push --force-with-lease origin fm/alpha-1",
        "git push --force-with-lease=fm/alpha-1:abc origin fm/alpha-1",
        "git push --force-if-includes origin fm/alpha-1",
        "git push origin +fm/alpha-1",
        "git push origin +HEAD:fm/alpha-1",
    ] {
        denied(Role::Builder, command, Code::DefaultPush);
    }
}

#[test]
fn push_to_other_branch_denied() {
    for command in [
        "git push origin HEAD:fm/other-card",
        "git push origin fm/other-card",
        "git push origin HEAD:refs/tags/v1",
        "git push --tags origin",
        "git push origin \"$TARGET\"",
        "gh-axi stack push",
        "gh-axi stack submit --open",
    ] {
        denied(Role::Builder, command, Code::ForeignBranch);
    }
}

#[test]
fn non_builder_push_denied() {
    for role in [
        Role::Orchestrator,
        Role::Scout,
        Role::Reviewer,
        Role::Verifier,
    ] {
        denied(role, "git push origin HEAD", Code::ForeignBranch);
        denied(role, "git push origin HEAD:fm/alpha-1", Code::ForeignBranch);
        denied(role, "git push no-mistakes fm/alpha-1", Code::ForeignBranch);
    }
}

#[test]
fn builder_push_to_its_own_branch_is_allowed() {
    for command in [
        "git push origin HEAD:fm/alpha-1",
        "git push -u origin HEAD:fm/alpha-1",
        "git push origin fm/alpha-1",
        "git push origin HEAD:refs/heads/fm/alpha-1",
        "git push no-mistakes HEAD:fm/alpha-1",
        "git -C /home/user/pool/3/alpha push origin HEAD:fm/alpha-1",
        "git push --set-upstream origin fm/alpha-1 --porcelain",
        "git commit -m 'never git push origin main' && git push origin HEAD:fm/alpha-1",
    ] {
        allowed(Role::Builder, command);
    }
    // No refspec, or a bare HEAD: the branch HEAD is on decides.
    let own = repo_on(BRANCH);
    for command in [
        "git push",
        "git push origin",
        "git push origin HEAD",
        "git push no-mistakes",
    ] {
        assert_eq!(
            bash_in(Role::Builder, own.path(), command),
            None,
            "{command}"
        );
    }
    let main = repo_on("main");
    let denial = bash_in(Role::Builder, main.path(), "git push origin HEAD").unwrap();
    assert_eq!(denial.code, Code::DefaultPush);
    let other = repo_on("fm/other");
    let denial = bash_in(Role::Builder, other.path(), "git push").unwrap();
    assert_eq!(denial.code, Code::ForeignBranch);
    // The facts are read only for a push, never for an ordinary command.
    let env = env();
    let read = std::cell::Cell::new(false);
    let load = || {
        read.set(true);
        facts()
    };
    let ctx = Context::new(Role::Builder, &env, PathBuf::from(WORKTREE), &load);
    assert_eq!(
        decide(
            "Bash",
            &json!({"command": "cargo test && git status"}),
            &ctx
        ),
        None
    );
    assert!(!read.get() && !ctx.read_facts());
}

#[test]
fn ordinary_git_and_quoted_text_pass() {
    for role in Role::ALL {
        for command in [
            "git -C /home/user/pool/3/alpha status",
            "git status",
            "git log --oneline -5",
            "git diff HEAD~1",
            "echo 'git push origin main'",
            "printf '%s\\n' \"gh pr merge 12\"",
            "grep -rn 'git push origin main' docs/",
            "git config --get user.name",
            "git config user.email",
            "git -c color.ui=never log",
            "git worktree list",
            "cat <<'EOF'\ngit push origin main\nEOF",
        ] {
            allowed(role, command);
        }
    }
    for command in [
        "git commit -m \"fix: note that ~/.tiller/state.db stays put\"",
        "cargo nextest run --workspace",
        "nix develop -c cargo clippy --workspace --all-targets -- -D warnings",
        "bun run --cwd board test",
        "(cd /home/user/pool/3/alpha && make)",
        "kill %1",
        "sleep 30 & pid=$!; kill $pid",
        "ls -la .claude/",
        "cat .claude/settings.json",
        "rg '.tiller' crates/",
    ] {
        allowed(Role::Builder, command);
    }
}

// -- G15 and G16: the orchestrator --------------------------------------------

#[test]
fn orchestrator_persistent_cd_into_checkout_is_denied() {
    for command in [
        "cd /home/user/checkouts/alpha",
        "cd /home/user/pool/3/alpha && git status",
        "pushd /home/user/pool/3/alpha",
        // Split so the repo scrub does not read it as a personal path.
        concat!("cd ~", "/checkouts/alpha"),
        "cd \"$HOME/pool/3/alpha\"",
        "cd ../../checkouts/alpha",
        "builtin cd /home/user/checkouts/alpha",
        "cd \"$(git rev-parse --show-toplevel)\"",
        "eval 'cd /home/user/checkouts/alpha'",
        // Option clusters and `pushd -n`, which stacks it for `popd`.
        "cd -LP /home/user/checkouts/alpha",
        "cd -P -e -- /home/user/checkouts/alpha",
        "pushd -n /home/user/pool/3/alpha",
    ] {
        denied(Role::Orchestrator, command, Code::PersistentCd);
    }
    for command in [
        "(cd /home/user/checkouts/alpha && git status)",
        "git -C /home/user/checkouts/alpha status",
        "cd /home/user/tiller-homes/alpha",
        "cd",
        "cd /tmp && ls",
        "bash -c 'cd /home/user/checkouts/alpha && git log'",
        "cd /home/user/checkouts/alpha | true",
        "cd /home/user/checkouts/alpha &",
        "cd -LP /tmp",
        "cd -",
        "pushd +1",
        "popd",
    ] {
        allowed(Role::Orchestrator, command);
    }
    // Only the orchestrator's shell is guarded this way.
    allowed(Role::Builder, "cd /home/user/pool/3/alpha");
}

#[test]
fn orchestrator_subagent_spawn_is_denied() {
    for name in [
        "Agent",
        "Task",
        "SendMessage",
        "Workflow",
        "CronCreate",
        "Monitor",
    ] {
        let denial = tool(
            Role::Orchestrator,
            name,
            json!({"subagent_type": "Explore"}),
        )
        .expect(name);
        assert_eq!(denial.code, Code::Delegation, "{name}");
    }
    for name in [
        "TaskCreate",
        "TaskUpdate",
        "TaskOutput",
        "TaskStop",
        "TaskList",
        "TaskGet",
        "CronList",
        "BashOutput",
        "KillShell",
        "Read",
        "Edit",
        "Glob",
        "Grep",
        "WebFetch",
        "TodoWrite",
        "AskUserQuestion",
    ] {
        let input = json!({"file_path": "/home/user/tiller-homes/alpha/notes.md"});
        assert_eq!(tool(Role::Orchestrator, name, input), None, "{name}");
    }
}

#[test]
fn delegation_shaped_future_tool_is_denied() {
    for name in [
        "SpawnWorker",
        "DispatchTask",
        "Handoff",
        "RemoteRun",
        "spawn_agent",
        "DelegateWork",
        "ScheduleJob",
        "WorktreeAdd",
    ] {
        let denial = tool(Role::Orchestrator, name, json!({})).expect(name);
        assert_eq!(denial.code, Code::Delegation, "{name}");
    }
    // MCP tools are classified for forge writes, not delegation stems.
    assert_eq!(
        tool(Role::Orchestrator, "mcp__notes__spawn_note", json!({})),
        None
    );
}

#[test]
fn worker_subagents_follow_the_role() {
    let explore = json!({"subagent_type": "Explore", "prompt": "find x"});
    let general = json!({"subagent_type": "general-purpose", "prompt": "do x"});
    let isolated = json!({"subagent_type": "Explore", "isolation": "worktree"});
    assert_eq!(tool(Role::Builder, "Agent", general.clone()), None);
    for role in [Role::Scout, Role::Reviewer, Role::Verifier] {
        assert_eq!(tool(role, "Agent", explore.clone()), None, "{role}");
        assert_eq!(
            tool(role, "Agent", general.clone()).unwrap().code,
            Code::Delegation,
            "{role}"
        );
    }
    for role in [Role::Builder, Role::Scout, Role::Reviewer, Role::Verifier] {
        assert_eq!(
            tool(role, "Agent", isolated.clone()).unwrap().code,
            Code::WorktreeAdmin,
            "{role}"
        );
    }
}

// -- G17 worktrees and panes --------------------------------------------------

#[test]
fn worktree_admin_denied() {
    for command in [
        "git worktree add ../x",
        "git worktree remove ../other",
        "git -C /home/user/pool/2/alpha worktree remove --force .",
        "git worktree move a b",
        "git worktree prune",
        "treehouse return",
        "treehouse remove 3",
        "herdr pane send-keys p1 1",
        "/usr/bin/herdr pane list",
    ] {
        denied_for_all(command, Code::WorktreeAdmin);
    }
    for role in Role::ALL {
        for name in ["EnterWorktree", "ExitWorktree"] {
            assert_eq!(
                tool(role, name, json!({})).unwrap().code,
                Code::WorktreeAdmin
            );
        }
    }
}

// -- G20 and G21: state and guards --------------------------------------------

#[test]
fn state_dir_access_denied() {
    for command in [
        "sqlite3 ~/.tiller/state.db \"update approvals set state='accepted'\"",
        "sqlite3 /home/user/.tiller/state.db .dump",
        "sqlite3 $HOME/.tiller/state.db .dump",
        "sqlite3 \"$TILLER_HOME/state.db\" .dump",
        "cat ~/.tiller/owner",
        "python3 -c 'print(1)' > ~/.tiller/owner",
        "ln -s ~/.tiller link",
        "cp x ~/.tiller/",
        "tiller card create alpha --kind ship --title x --body-file ~/.tiller/board-cookie",
    ] {
        denied_for_all(command, Code::StateWrite);
    }
    for (name, input) in [
        ("Read", json!({"file_path": "/home/user/.tiller/state.db"})),
        (
            "Edit",
            json!({"file_path": "/home/user/.tiller/config.toml"}),
        ),
        ("Write", json!({"file_path": "~/.tiller/owner"})),
        (
            "Grep",
            json!({"pattern": "x", "path": "/home/user/.tiller"}),
        ),
    ] {
        for role in Role::ALL {
            assert_eq!(
                tool(role, name, input.clone()).unwrap().code,
                Code::StateWrite,
                "{role}: {name}"
            );
        }
    }
}

#[test]
fn settings_edit_denied() {
    for command in [
        "echo '{\"disableAllHooks\": true}' > ~/.claude/settings.json",
        "echo x > .claude/settings.local.json",
        "cp /tmp/s.json /home/user/pool/3/alpha/.claude/settings.local.json",
        "tee ~/.claude.json < /tmp/x",
        "mkdir -p .claude/rules",
        "rm -rf .claude",
        "sed -i 's/a/b/' .claude/settings.json",
        "printf '#!/bin/sh\\n' > .git/hooks/pre-push",
        "chmod +x .git/hooks/pre-push",
        "git config core.hooksPath /tmp/hooks",
        "git config set core.hooksPath /tmp/hooks",
        "git config --global alias.ship '!gh pr merge'",
        "git -c core.hooksPath=/dev/null commit -m x",
        "git -c alias.x='!sh' x",
        "git config --edit",
        "GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=core.hooksPath GIT_CONFIG_VALUE_0=/dev/null git commit",
    ] {
        denied_for_all(command, Code::GuardTamper);
    }
    for (name, path) in [
        ("Edit", "/home/user/.claude/settings.json"),
        (
            "Write",
            "/home/user/pool/3/alpha/.claude/settings.local.json",
        ),
        ("Edit", "/home/user/pool/3/alpha/.claude/rules/style.md"),
        ("Write", "/home/user/pool/3/alpha/.git/hooks/pre-commit"),
        ("Edit", "/home/user/.claude.json"),
    ] {
        for role in Role::ALL {
            let denial = tool(role, name, json!({"file_path": path})).unwrap();
            assert_eq!(denial.code, Code::GuardTamper, "{role}: {name} {path}");
        }
    }
    // Project memory files are the builder's to edit (C01 as amended by S04).
    assert_eq!(
        tool(
            Role::Builder,
            "Edit",
            json!({"file_path": "/home/user/pool/3/alpha/AGENTS.md"})
        ),
        None
    );
}

// -- G22 and G23: owner commands and identity ---------------------------------

#[test]
fn owner_tiller_commands_denied() {
    for command in [
        "tiller approve 01JABC",
        "tiller approve 01JABC --reject",
        "tiller approve 01JABC --sha abcdef0",
        "tiller grant create alpha --kind merge",
        "tiller away start --until tomorrow --projects alpha",
        "tiller land 01JABC",
        "tiller board",
        "tiller board --login",
        "tiller import --from x",
        "echo '{\"session_id\":\"x\"}' | tiller hook UserPromptSubmit",
        "tiller guard pretool --role builder",
        "tiller owner set firstmate",
        "tiller init",
        "tiller backup",
        "tiller export --to /tmp/x",
        "tiller restore /tmp/x",
        "tiller migrate down",
        "tiller config set stop_hook_mode off",
        "tiller project add beta --name Beta",
        "tiller project stage alpha pre-launch --rev 3",
        "tiller project stage alpha launched --rev 4",
        "tiller host edit --lane-capacity 9",
        "tiller daemon",
        "tiller --json approve 01JABC",
        "/nix/store/abc-tiller-0.1.0/bin/tiller grant revoke 01JABC",
        "sh -c 'tiller approve 01JABC'",
        "tiller $SUB 01JABC",
    ] {
        denied_for_all(command, Code::OwnerOnly);
    }
    for role in Role::ALL {
        for command in [
            "tiller status working: tests green",
            "tiller --json snapshot",
            "tiller card show alpha-1",
            "tiller project list",
            "tiller config get stop_hook_mode",
            "tiller migrate up",
            "/nix/store/abc-tiller-0.1.0/bin/tiller events --limit 5",
        ] {
            allowed(role, command);
        }
    }
}

// -- G43: the peer link --------------------------------------------------------

#[test]
fn peer_commands_denied() {
    for command in [
        "tiller peer list",
        "tiller peer add host-b --alias host-b --scope act",
        "tiller peer accept --from host-a --key abc",
        "tiller peer serve --from host-a",
        "tiller peer revoke 01JABC",
        "tiller --json peer remove host-b",
        "/nix/store/abc-tiller-0.1.0/bin/tiller peer list",
        "sh -c 'tiller peer list'",
        "echo host-b | xargs tiller peer remove",
    ] {
        denied_for_all(command, Code::Peer);
    }
}

#[test]
fn ssh_remote_tiller_denied() {
    for command in [
        "ssh host-b tiller approve 01JABC",
        "ssh -t host-b tiller peer accept --from host-a",
        "sh -c 'ssh host-b tiller card close alpha-1'",
        "ssh host-b 'tiller card comment alpha-1 hi'",
        "ssh -p 2222 user@192.0.2.10 -- /home/user/.nix-profile/bin/tiller status",
        "ssh host-b \"cd /home/user && tiller approve 01JABC\"",
        "mosh host-b -- tiller approve 01JABC",
        "et host-b -c 'tiller land 01JABC'",
        "ssh host-b $CMD",
        "ssh host-b ls \"$DIR\"",
        "echo 'tiller approve 01JABC' | ssh host-b",
        "echo 'tiller approve 01JABC' | ssh host-b bash -s",
        "ssh host-b sh < cmds.txt",
        "ssh host-b <<'EOF'\ntiller approve 01JABC\nEOF",
        "ssh host-b \"ti''ller approve 01JABC\"",
        "ssh host-b 'X=tiller; $X approve 01JABC'",
        "ssh host-b 'eval \"$(printf til)ler approve\"'",
        "ssh host-b 'ssh host-c tiller approve 01JABC'",
        "ssh -o RemoteCommand='tiller status' host-b",
        "ssh -oProxyCommand='tiller peer serve --from host-a' host-b",
        "ssh -o \"$OPTS\" host-b",
        "ssh --weird host-b",
        "mosh --server='tiller peer serve' host-b",
        "mosh --ssh 'ssh host-b tiller approve 01JABC' host-b",
        "et host-b --command 'tiller land 01JABC'",
    ] {
        denied_for_all(command, Code::Peer);
    }
    for role in Role::ALL {
        for command in [
            "ssh -V",
            "ssh -G host-b",
            "echo ssh host-b tiller approve",
            "ssh host-b ls /opt/tiller",
            "ssh tiller uptime",
            "ssh \"$HOST\" uptime",
            "ssh -i \"$KEY\" host-b ls",
            "ssh -p 2222 -o ServerAliveInterval=30 host-b uptime",
            "ssh host-b -l user uptime",
            "ssh host-b <<'EOF'\nuptime\nEOF",
            "echo report | ssh host-b 'cat > report.txt'",
            "mosh host-b",
            "mosh -p 60001 host-b -- htop",
            "et -p 2022 host-b",
        ] {
            allowed(role, command);
        }
    }
}

/// CP2 decision on G06: the orchestrator may run exactly
/// `tiller approve <id> --ask <ask>`, and every other approve form stays
/// denied. `tiller` refuses the bare form too (the CLI test
/// `chat_approval_needs_a_newer_prompt_from_the_calling_orchestrator`).
#[test]
fn orchestrator_bare_approve_is_denied() {
    for command in [
        "tiller approve 01JABC --ask 01JASK",
        "tiller approve 01JABC --ask=01JASK",
        "tiller --json approve 01JABC --ask 01JASK",
        "tiller approve --ask 01JASK 01JABC",
    ] {
        allowed(Role::Orchestrator, command);
        for role in [Role::Scout, Role::Builder, Role::Reviewer, Role::Verifier] {
            denied(role, command, Code::OwnerOnly);
        }
    }
    for command in [
        "tiller approve 01JABC",
        "tiller approve 01JABC --reject --ask 01JASK",
        "tiller approve 01JABC --ask 01JASK --key k1",
        "tiller approve 01JABC 01JDEF --ask 01JASK",
        "tiller approve 01JABC --ask",
        "tiller approve 01JABC --ask \"$ASK\"",
        "tiller approve 01JABC --ask 01JASK --ask 01JASK2",
        "echo 01JASK | xargs tiller approve 01JABC --ask",
        "echo approve 01JABC | xargs tiller",
        "echo down | xargs tiller migrate",
    ] {
        denied(Role::Orchestrator, command, Code::OwnerOnly);
    }
}

#[test]
fn identity_stripping_denied() {
    for command in [
        "env -u TILLER_SESSION_ID -u CLAUDECODE script -qc 'tiller approve 01JABC'",
        "env -u TILLER_SESSION_ID tiller status x",
        "env --unset=CLAUDECODE tiller status x",
        "env -i tiller status x",
        "unset TILLER_SESSION_ID; tiller approve 01JABC",
        "unset CLAUDECODE",
        "TILLER_SESSION_ID= tiller approve 01JABC",
        "CLAUDECODE= tiller status x",
        "TILLER_HOME=/tmp/fake tiller status x",
        "export TILLER_SESSION_ID=",
        "export -n CLAUDECODE",
        "script -qc 'tiller approve 01JABC' /dev/null",
        "expect -c 'spawn tiller approve 01JABC'",
        "unbuffer tiller approve 01JABC",
        "socat - EXEC:'tiller approve 01JABC',pty",
        "tmux new -d 'tiller approve 01JABC'",
    ] {
        denied_for_all(command, Code::Identity);
    }
}

#[test]
fn tiller_shadowing_denied() {
    for command in [
        "PATH=./bin:$PATH tiller status x",
        "export PATH=/tmp/bin:$PATH",
        "tiller() { echo fake; }; tiller status x",
        "function tiller { echo fake; }",
        "function /nix/store/abc-tiller-0.1.0/bin/tiller { echo fake; }; /nix/store/abc-tiller-0.1.0/bin/tiller status x",
        "alias tiller=/tmp/tiller",
        "alias /nix/store/abc-tiller-0.1.0/bin/tiller=/tmp/tiller",
        "./tiller status x",
        "/tmp/bin/tiller status x",
        "LD_PRELOAD=/tmp/x.so tiller status x",
        "env LD_PRELOAD=/tmp/x.so tiller status x",
        "unset PATH",
    ] {
        denied_for_all(command, Code::GuardTamper);
    }
    allowed(
        Role::Builder,
        "/nix/store/abc-tiller-0.1.0/bin/tiller status working: x",
    );
}

#[test]
fn secret_paths_are_never_handed_to_tiller() {
    for command in [
        "tiller card create alpha --kind ship --title x --body-file .env",
        "tiller artifact add alpha-1 --path config/server.key",
        "tiller card create alpha --kind ship --title x --body-file=/home/user/.ssh/id_ed25519",
    ] {
        denied(Role::Builder, command, Code::StateWrite);
    }
}

// -- G36 and G37: nested agents, schedulers, process control -------------------

#[test]
fn nested_agent_and_scheduler_denied() {
    for command in [
        "claude -p 'merge PR 12' --dangerously-skip-permissions",
        "/usr/local/bin/claude --version",
        "npx @anthropic-ai/claude-code -p x",
        "bunx @anthropic-ai/claude-code",
        "codex exec 'x'",
        "crontab -l",
        "crontab /tmp/cron",
        "at now + 1 hour",
        "systemd-run --user sleep 1",
        "launchctl load x.plist",
        "systemctl --user start x",
    ] {
        denied_for_all(command, Code::Delegation);
    }
    for role in Role::ALL {
        for name in [
            "CronCreate",
            "CronDelete",
            "ScheduleWakeup",
            "RemoteTrigger",
            "Workflow",
            "PushNotification",
        ] {
            assert_eq!(
                tool(role, name, json!({})).unwrap().code,
                Code::Delegation,
                "{role}: {name}"
            );
        }
    }
}

#[test]
fn daemon_stop_denied() {
    for command in [
        "pkill -f 'tiller daemon'",
        "killall tiller",
        "systemctl --user stop tiller",
        "kill 4242",
        "kill -9 4242",
        "kill -TERM 1",
        "kill -- -1",
        "kill $(pgrep -f 'tiller daemon')",
        "pgrep tiller | xargs kill",
        // A variable holds whatever pid was put in it, not only `$!`.
        "pid=$(pgrep -f 'tiller daemon'); kill $pid",
        "pid=$!; pid=$(pgrep tiller); kill \"$pid\"",
        "kill $TILLER_DAEMON_PID",
        "kill ${PID}",
        "kill %$(pgrep tiller)",
    ] {
        denied_for_all(command, Code::GuardTamper);
    }
    for command in [
        "kill %1",
        "kill %%",
        "kill $!",
        "sleep 30 & pid=$!; kill \"$pid\"",
        "sleep 30 & pid=${!}; kill -TERM ${pid}",
        "kill -l",
    ] {
        allowed(Role::Builder, command);
    }
}

// -- G38 the record -----------------------------------------------------------

#[test]
fn denial_writes_observation() {
    use tiller_core::domain::{Boundary, Delivery, Observation};
    use tiller_core::state::list;
    use tiller_core::state::projects::{Init, NewProject};
    use tiller_core::state::runs::NewOrchestrator;

    let home = tempfile::tempdir().unwrap();
    let db = home.path().join("state.db");
    let mut store = Store::open(&db).unwrap();
    let owner = Ctx::new(Actor::Owner);
    store
        .init(
            &owner,
            Init {
                host_label: "test-host".into(),
                lane_capacity: 2,
                ram_floor_mb: None,
                disk_floor_mb: None,
                boundary: Boundary::Personal,
            },
        )
        .unwrap();
    store
        .project_add(
            &owner,
            NewProject {
                id: "alpha".into(),
                name: "Alpha".into(),
                repo: Some("example/alpha".into()),
                remote: None,
                default_branch: Some("main".into()),
                delivery: Delivery::DirectPr,
                checks: None,
                capacity: Some(2),
                deploy_workflow: None,
                deploy_environment: None,
                deploy_timeout_minutes: None,
            },
        )
        .unwrap();
    let session = store
        .session_start(
            &owner,
            NewOrchestrator {
                project_id: "alpha".into(),
                claude_session_id: Some("claude-1".into()),
                ..NewOrchestrator::default()
            },
        )
        .unwrap()
        .value
        .session;
    drop(store);

    let env = Env {
        tiller_home: home.path().to_path_buf(),
        tiller_session_id: Some(session.id.clone()),
        ..env()
    };
    let payload = |command: &str| {
        json!({
            "session_id": "claude-1",
            "cwd": "/home/user/tiller-homes/alpha",
            "hook_event_name": "PreToolUse",
            "tool_name": "Bash",
            "tool_input": {"command": command, "description": "secret words"},
        })
        .to_string()
    };
    let denial = pretool(
        Role::Orchestrator,
        payload("gh pr merge 12").as_bytes(),
        &env,
    )
    .unwrap();
    assert_eq!(denial.code, Code::Merge);
    assert_eq!(
        pretool(
            Role::Orchestrator,
            payload("tiller snapshot").as_bytes(),
            &env
        ),
        None
    );

    let store = Store::open(&db).unwrap();
    let rows: Vec<Observation> = list(store.conn(), "event = 'PreToolUse:denied'", &[]).unwrap();
    assert_eq!(rows.len(), 1, "one row per denial, none for an allow");
    let row = &rows[0];
    assert_eq!(row.session_id.as_deref(), Some(session.id.as_str()));
    assert_eq!(row.payload["code"], "merge");
    assert_eq!(row.payload["role"], "orchestrator");
    assert_eq!(row.payload["command"], "gh pr merge 12");
    // Never the full payload.
    assert!(!row.payload.to_string().contains("secret words"));
}

#[test]
fn deny_transport_names_the_code_and_the_allowed_path() {
    let denial = bash(Role::Builder, "git push origin main").unwrap();
    let rendered: Value = serde_json::from_str(&render_deny(&denial)).unwrap();
    let output = &rendered["hookSpecificOutput"];
    assert_eq!(output["hookEventName"], "PreToolUse");
    assert_eq!(output["permissionDecision"], "deny");
    let reason = output["permissionDecisionReason"].as_str().unwrap();
    assert!(reason.starts_with("[default-push] "), "{reason}");
    assert!(
        reason.contains("git push origin HEAD:<your branch>"),
        "{reason}"
    );
    assert_eq!(rendered["systemMessage"], reason);
}

#[test]
fn a_broken_payload_allows() {
    let env = env();
    assert_eq!(pretool(Role::Builder, b"", &env), None);
    assert_eq!(pretool(Role::Builder, b"{not json", &env), None);
    assert_eq!(pretool(Role::Builder, b"{\"tool_input\":{}}", &env), None);
}

// -- Fragment tests (section 12) ----------------------------------------------

fn fill(role: Role) -> Placeholders {
    let orchestrator = role == Role::Orchestrator;
    Placeholders {
        tiller: TILLER.into(),
        worktree: if orchestrator {
            "/home/user/checkouts/alpha"
        } else {
            WORKTREE
        }
        .into(),
        scratch: "/home/user/scratch/alpha-1".into(),
        tiller_home: "/home/user/.tiller".into(),
        home: if orchestrator {
            "/home/user/tiller-homes/alpha"
        } else {
            ""
        }
        .into(),
        pool: if orchestrator { "/home/user/pool" } else { "" }.into(),
        checkouts: if orchestrator {
            "/home/user/checkouts"
        } else {
            ""
        }
        .into(),
        default_branch: "main".into(),
        network_allow: vec!["github.com".into()],
        no_mistakes_socket: None,
        gh_login_readable: false,
    }
}

fn rendered(role: Role) -> Value {
    render_settings(role, &fill(role)).unwrap()
}

fn rules<'a>(value: &'a Value, list: &str) -> Vec<&'a str> {
    value["permissions"][list]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect()
}

#[test]
fn fragment_sets_sandbox_write_set() {
    for role in Role::ALL {
        let fragment = rendered(role);
        let sandbox = &fragment["sandbox"];
        assert_eq!(sandbox["enabled"], true, "{role}");
        assert_eq!(sandbox["failIfUnavailable"], true, "{role}");
        assert_eq!(sandbox["allowUnsandboxedCommands"], false, "{role}");
        let write: Vec<&str> = sandbox["filesystem"]["allowWrite"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        let expected: Vec<&str> = match role {
            Role::Orchestrator => vec![
                "/home/user/tiller-homes/alpha",
                "/home/user/scratch/alpha-1",
            ],
            Role::Reviewer => vec!["/home/user/scratch/alpha-1"],
            _ => vec![WORKTREE, "/home/user/scratch/alpha-1"],
        };
        assert_eq!(write, expected, "{role}: write set");
        let excluded: Vec<&str> = sandbox["excludedCommands"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert_eq!(
            excluded,
            [TILLER.to_owned(), format!("{TILLER} *")],
            "{role}: only the packaged tiller"
        );
    }
}

#[test]
fn fragment_edit_rules_match_write_set() {
    for role in Role::ALL {
        let fragment = rendered(role);
        let allow = rules(&fragment, "allow");
        let edits: Vec<&str> = allow
            .iter()
            .copied()
            .filter(|rule| rule.starts_with("Edit("))
            .collect();
        let expected: Vec<String> = match role {
            Role::Orchestrator => vec![
                "Edit(//home/user/tiller-homes/alpha/**)".into(),
                "Edit(//home/user/scratch/alpha-1/**)".into(),
            ],
            Role::Reviewer => vec![],
            _ => vec![
                format!("Edit(/{WORKTREE}/**)"),
                "Edit(//home/user/scratch/alpha-1/**)".into(),
            ],
        };
        assert_eq!(edits, expected, "{role}: Edit allow rules");
        if role.is_worker() {
            assert_eq!(fragment["permissions"]["defaultMode"], "dontAsk", "{role}");
        }
    }
    let reviewer = rendered(Role::Reviewer);
    for tool in ["Edit", "Write", "NotebookEdit"] {
        assert!(
            rules(&reviewer, "deny").contains(&tool),
            "reviewer keeps {tool}"
        );
    }
}

#[test]
fn orchestrator_edit_outside_home_is_denied() {
    let fragment = rendered(Role::Orchestrator);
    let deny = rules(&fragment, "deny");
    assert!(deny.contains(&"Edit(//home/user/checkouts/**)"));
    assert!(deny.contains(&"Edit(//home/user/pool/**)"));
    let guard = fragment["hooks"]["PreToolUse"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap();
    assert_eq!(
        guard,
        format!(
            "{TILLER} guard pretool --role orchestrator --checkouts '/home/user/checkouts' --pool '/home/user/pool'"
        )
    );
}

#[test]
fn secret_reads_denied() {
    for role in Role::ALL {
        let fragment = rendered(role);
        let deny = rules(&fragment, "deny");
        for rule in [
            "Read(**/.env)",
            "Read(**/.env.*)",
            "Read(**/.envrc)",
            "Read(**/*.pem)",
            "Read(**/*.key)",
            "Read(//home/user/.tiller/**)",
        ] {
            assert!(deny.contains(&rule), "{role}: {rule}");
        }
        let deny_read: Vec<&str> = fragment["sandbox"]["filesystem"]["denyRead"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert!(deny_read.contains(&"/home/user/.tiller"), "{role}");
    }
}

#[test]
fn approve_deny_rule_is_lifted_only_for_the_orchestrator() {
    for role in Role::ALL {
        let fragment = rendered(role);
        let deny = rules(&fragment, "deny");
        for rule in [
            "Bash(tiller approve *)".to_owned(),
            format!("Bash({TILLER} approve *)"),
        ] {
            assert_eq!(
                deny.contains(&rule.as_str()),
                role != Role::Orchestrator,
                "{role}: {rule}"
            );
        }
        for verb in ["grant", "land", "hook"] {
            for rule in [
                format!("Bash(tiller {verb} *)"),
                format!("Bash({TILLER} {verb} *)"),
            ] {
                assert!(deny.contains(&rule.as_str()), "{role}: {rule}");
            }
        }
    }
}

fn packaged(command: &str) -> Option<String> {
    let input = json!({ "command": command, "description": "Read the handoff" });
    let updated = packaged_form("Bash", &input, Some(Path::new(TILLER)))?;
    assert_eq!(updated["description"], "Read the handoff", "{command}");
    Some(updated["command"].as_str().unwrap().to_owned())
}

#[test]
fn bare_tiller_is_rewritten_to_the_packaged_path() {
    for (command, want) in [
        ("tiller handoff show", format!("{TILLER} 'handoff' 'show'")),
        ("tiller", TILLER.to_owned()),
        ("  tiller owner show", format!("{TILLER} 'owner' 'show'")),
        ("tiller\tcard list", format!("{TILLER} 'card' 'list'")),
        (
            "tiller status working 'a line; with | marks'",
            format!("{TILLER} 'status' 'working' 'a line; with | marks'"),
        ),
        (
            "tiller status working \"it's done\"",
            format!("{TILLER} 'status' 'working' 'it'\\''s done'"),
        ),
        ("tiller card list # mine", format!("{TILLER} 'card' 'list'")),
        (
            "tiller status working 'two\nlines'",
            format!("{TILLER} 'status' 'working' 'two\nlines'"),
        ),
        (
            "tiller card list --since=2026-11-04",
            format!("{TILLER} 'card' 'list' '--since=2026-11-04'"),
        ),
        (
            "tiller config path ~",
            format!("{TILLER} 'config' 'path' ~"),
        ),
        (
            "tiller import ~/.cache/bundle.json",
            format!("{TILLER} 'import' ~/'.cache/bundle.json'"),
        ),
    ] {
        assert_eq!(packaged(command), Some(want), "{command}");
    }
}

/// A global alias in the owner's zsh snapshot (`alias -g G='| grep'`) or an
/// `=word` expands only in an unquoted word, so every argument is quoted:
/// otherwise the exempt call could run a pipe outside the sandbox.
#[test]
fn rewrite_quotes_words_zsh_would_expand() {
    assert_eq!(
        packaged("tiller card list G token"),
        Some(format!("{TILLER} 'card' 'list' 'G' 'token'"))
    );
    assert_eq!(
        packaged("tiller card list =cat"),
        Some(format!("{TILLER} 'card' 'list' '=cat'"))
    );
}

/// The packaged path typed by hand gets the same quoting, so a role that
/// types it cannot reach an unquoted global alias either.
#[test]
fn a_lone_packaged_call_is_requoted() {
    assert_eq!(
        packaged(&format!("{TILLER} handoff show")),
        Some(format!("{TILLER} 'handoff' 'show'"))
    );
    assert_eq!(packaged(TILLER), Some(TILLER.to_owned()));
    for command in [
        format!("cd /tmp && {TILLER} handoff show"),
        format!("{TILLER}x handoff show"),
        format!("'{TILLER}' handoff show"),
        format!("{TILLER} handoff show > /home/user/out"),
    ] {
        assert_eq!(packaged(&command), None, "{command}");
    }
}

#[test]
fn only_a_lone_literal_tiller_is_rewritten() {
    for command in [
        "cd /tmp && tiller handoff show",
        "tiller handoff show | cat",
        "tiller handoff show && cat ~/.tiller/state.db",
        "tiller handoff show; cat ~/.tiller/state.db",
        "tiller handoff show\ncat ~/.tiller/state.db",
        "tiller handoff show &",
        "(tiller handoff show)",
        "{ tiller handoff show; }",
        "FOO=1 tiller handoff show",
        "tiller handoff show > /home/user/out",
        "tiller handoff show 2>&1",
        "tiller handoff show <<< x",
        "tiller \"$(touch /home/user/out)\"",
        "tiller $(touch /home/user/out)",
        "tiller `touch /home/user/out`",
        "tiller $VAR",
        "tiller *",
        "\"tiller\" handoff show",
        "\\tiller handoff show",
        "tillerx handoff show",
        "tiller-x handoff show",
        "tiller() { cat ~/.tiller/state.db; }; tiller",
        "/home/user/bin/tiller handoff show",
        "command tiller handoff show",
        "tiller config path ~user",
        "tiller status working $'\\xe2\\x82\\xac'",
        "tiller status working $\"a b\"",
        "tiller status working $'\\0'",
        "tiller status working 'a\rb'",
        "tiller 'unterminated",
    ] {
        assert_eq!(packaged(command), None, "{command}");
    }
}

#[test]
fn rewrite_needs_bash_and_a_plain_packaged_path() {
    let input = json!({ "command": "tiller handoff show" });
    assert_eq!(packaged_form("Read", &input, Some(Path::new(TILLER))), None);
    assert_eq!(packaged_form("Bash", &input, None), None);
    for path in [
        "bin/tiller",
        "/",
        "/home/user/bin/",
        "/home/user/my tiller/tiller",
        "/home/user/*/tiller",
        "/home/user/a?b/tiller",
        "/home/user/$(x)/tiller",
        "/home/user/a;b/tiller",
        "/home/user/'q'/tiller",
    ] {
        assert_eq!(
            packaged_form("Bash", &input, Some(Path::new(path))),
            None,
            "{path}"
        );
    }
    for path in [
        "/nix/store/abc-tiller-0.1.0/bin/tiller",
        "/home/user/src/tiller@main/target/debug/tiller",
        "/home/user/a,b:c%d=e/tiller",
    ] {
        assert!(plain_path(path), "{path}");
    }
}

#[test]
fn rewrite_answer_carries_only_the_updated_input() {
    let updated = json!({ "command": format!("{TILLER} handoff show") });
    let answer: Value = serde_json::from_str(&render_rewrite(&updated)).unwrap();
    assert_eq!(
        answer,
        json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "updatedInput": updated,
            },
        })
    );
}

#[test]
fn tiller_rules_name_both_forms() {
    for role in Role::ALL {
        let fragment = rendered(role);
        let deny = rules(&fragment, "deny");
        for rule in deny.iter().filter(|rule| rule.starts_with("Bash(tiller ")) {
            let packaged = rule.replacen("Bash(tiller ", &format!("Bash({TILLER} "), 1);
            assert!(deny.contains(&packaged.as_str()), "{role}: {packaged}");
        }
        let allow = rules(&fragment, "allow");
        assert!(
            !allow.iter().any(|rule| rule.starts_with("Bash(tiller")),
            "{role}: a bare allow never matches the rewritten call"
        );
    }
    let orchestrator = rendered(Role::Orchestrator);
    let allow = rules(&orchestrator, "allow");
    assert!(allow.contains(&format!("Bash({TILLER} *)").as_str()));
}
