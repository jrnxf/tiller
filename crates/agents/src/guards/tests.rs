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

/// T6: `gh` reads short flags in clusters, so the guard does too.
#[test]
fn gh_short_flag_clusters_read_like_gh() {
    const CASES: &[(&str, Option<Code>)] = &[
        ("gh pr close 5 -d", Some(Code::Merge)),
        ("gh pr close 5 -dc", Some(Code::Merge)),
        ("gh pr close 5 -dc 'superseded'", Some(Code::Merge)),
        ("gh pr close -d=true 5", Some(Code::Merge)),
        ("gh pr close 5 --delete-branch=true", Some(Code::Merge)),
        ("gh-axi pr close 5 -dc x", Some(Code::Merge)),
        (
            "gh repo edit --default-branch=release",
            Some(Code::DefaultPush),
        ),
        // `-c` and `-R` take the rest of the cluster as their value.
        ("gh pr close 5 -cd", None),
        ("gh pr close 5 -c 'done' -R example/alpha", None),
        ("gh pr close 5 -Rexample/dash-d", None),
        ("gh pr close 5", None),
    ];
    for role in Role::ALL {
        for (command, verdict) in CASES {
            match verdict {
                Some(code) => denied(role, command, *code),
                None => allowed(role, command),
            }
        }
    }
}

#[test]
fn pr_approve_retarget_and_release_delete_denied() {
    for command in [
        "gh pr review 12 --approve",
        "gh pr review --approve 12",
        "gh pr review 12 --approve=true",
        "gh pr review 12 -a",
        "gh pr review 12 -a -b lgtm",
        "gh pr review 12 -ab lgtm",
        "gh pr review 12 -ca",
        "gh-axi pr review 12 --approve",
        "gh -R example/alpha pr review 12 -a",
        "gh pr review 12 $FLAG",
        "echo --approve | xargs gh pr review 12",
        "gh pr edit 12 --base main",
        "gh pr edit 12 --base=main",
        "gh pr edit 12 -B main",
        "gh pr edit 12 -Bmain",
        "gh pr edit 12 --base refs/heads/main",
        "gh pr edit 12 --title x --base main",
        "gh-axi pr edit 12 --base main",
        "gh pr edit 12 --base \"$BASE\"",
        "gh pr edit 12 --base",
        "gh release delete v1.0.0",
        "gh release delete v1.0.0 --yes --cleanup-tag",
        "gh-axi release delete v1.0.0",
        "gh -R example/alpha release delete v1.0.0",
        "gh release delete-asset v1.0.0 tiller.tar.gz",
        "sh -c 'gh pr review 12 --approve'",
        "gh api repos/o/r/pulls/12/reviews -f event=APPROVE",
        "gh api -X PATCH repos/o/r/pulls/12 -f base=main",
        "gh api -X DELETE repos/o/r/releases/1",
    ] {
        denied_for_all(command, Code::Merge);
    }
    let denial = bash(Role::Builder, "gh pr review 12 --approve").unwrap();
    assert!(
        denial.message().contains("approves a PR")
            && denial
                .message()
                .contains("ask the orchestrator to request a merge approval"),
        "{}",
        denial.message()
    );
    for command in [
        "gh pr review 12 --comment -b 'looks fine'",
        "gh pr review 12 --request-changes --body 'see line 4'",
        "gh pr review 12 -c -b -a",
        "gh pr review 12 -r -b approve",
        "gh pr edit 12 --base fm/alpha-0",
        "gh pr edit 12 -B fm/alpha-0 --title 'merge into main'",
        "gh pr edit 12 --title 'retarget --base main' --add-label ready",
        "gh-axi pr edit 12 --body x",
        "gh release view v1.0.0",
        "gh release list",
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
        "curl -X POST http://127.0.0.1:7421/api/v1/peers -d '{\"label\":\"host-b\",\"ssh\":\"host-b\"}'",
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

/// The merge App's key sits in the Tiller home (`authority.md`, Enforced
/// merge gate): no fleet role reads it, by any route.
#[test]
fn merge_app_key_denied() {
    for command in [
        "cat ~/.tiller/forge-app/private-key.pem",
        "base64 /home/user/.tiller/forge-app/private-key.pem",
        "cp ~/.tiller/forge-app/private-key.pem /tmp/k",
        "openssl rsa -in \"$TILLER_HOME/forge-app/private-key.pem\" -text",
        "ls ~/.tiller/forge-app",
    ] {
        denied_for_all(command, Code::StateWrite);
    }
    for (name, input) in [
        (
            "Read",
            json!({"file_path": "/home/user/.tiller/forge-app/private-key.pem"}),
        ),
        (
            "Grep",
            json!({"pattern": "PRIVATE", "path": "/home/user/.tiller/forge-app"}),
        ),
        ("Write", json!({"file_path": "~/.tiller/forge-app/app-id"})),
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
        "tiller owner set tiller",
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

/// T3: the owner commands the CLI gates on `require_owner` that the guard
/// once let through, next to their siblings any role may run.
#[test]
fn owner_retro_learn_and_agent_commands_denied() {
    const CASES: &[(&str, Option<Code>)] = &[
        ("tiller retro run alpha", Some(Code::OwnerOnly)),
        ("tiller learn eval 01JABC", Some(Code::OwnerOnly)),
        ("tiller agent restart --stale", Some(Code::OwnerOnly)),
        ("tiller agent focus alpha-1", Some(Code::OwnerOnly)),
        (
            "tiller --json agent restart --stale --dry-run",
            Some(Code::OwnerOnly),
        ),
        ("sh -c 'tiller agent focus alpha-1'", Some(Code::OwnerOnly)),
        ("echo run | xargs tiller retro", Some(Code::OwnerOnly)),
        // A second word not known before the command runs may be the
        // owner's one, so it is denied like an unseen one.
        ("tiller agent \"$SUB\" alpha-1", Some(Code::OwnerOnly)),
        ("tiller retro $(echo run) 01JABC", Some(Code::OwnerOnly)),
        ("tiller learn $VERB 01JABC", Some(Code::OwnerOnly)),
        ("tiller mem \"$X\" --class rule", Some(Code::OwnerOnly)),
        ("tiller config ${OP} k v", Some(Code::OwnerOnly)),
        ("tiller migrate \"$WAY\"", Some(Code::OwnerOnly)),
        // The CLI refuses these to every session (`refuse_fleet`), or core
        // takes them only from the owner or the daemon.
        ("tiller retro list", Some(Code::OwnerOnly)),
        ("tiller retro show 01JABC", Some(Code::OwnerOnly)),
        ("tiller retro sweep", Some(Code::OwnerOnly)),
        ("tiller learn status", Some(Code::OwnerOnly)),
        (
            "tiller learn bundle placement 01JABC",
            Some(Code::OwnerOnly),
        ),
        ("tiller learn add placement 01JABC", Some(Code::OwnerOnly)),
        ("tiller learn abandon placement", Some(Code::OwnerOnly)),
        ("tiller learn gate 01JABC", None),
        ("tiller agent relaunch alpha-1", None),
        ("tiller agent interrupt \"$SESSION\"", None),
        ("tiller mem note alpha --text \"$T\"", None),
        ("tiller config get \"$KEY\"", None),
    ];
    for role in Role::ALL {
        for (command, verdict) in CASES {
            match verdict {
                Some(code) => denied(role, command, *code),
                None => allowed(role, command),
            }
        }
    }
}

/// Every call site of a gate that refuses a command to the fleet, as
/// `(label, file)`: `require_owner` and `refuse_fleet` in the CLI, and
/// `require_owner_or_daemon` in core's learning state. Each call passes a
/// string literal that names the command; a call that does not is a failure,
/// so a new owner command cannot hide from the cross-check.
fn cli_owner_labels() -> Vec<(String, String)> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                out.push(path);
            }
        }
    }
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files = Vec::new();
    walk(&crates.join("cli/src"), &mut files);
    files.push(crates.join("core/src/state/learning.rs"));
    let mut labels = Vec::new();
    for file in files {
        let source = fs::read_to_string(&file).unwrap();
        let shown = file.strip_prefix(&crates).unwrap().display().to_string();
        let gates: &[&str] = if shown.starts_with("cli/") {
            &["require_owner(", "refuse_fleet("]
        } else {
            &["require_owner_or_daemon("]
        };
        for gate in gates {
            for (at, _) in source.match_indices(gate) {
                if source[..at].ends_with("fn ") {
                    continue;
                }
                // The label is the call's first string literal.
                let call = &source[at + gate.len()..];
                let call = &call[..call.find(';').unwrap_or(call.len())];
                let label = call
                    .split_once('"')
                    .and_then(|(_, rest)| rest.split_once('"'))
                    .map(|(label, _)| label.to_owned())
                    .unwrap_or_else(|| panic!("{shown}: {gate} takes a string literal"));
                labels.push((label, shown.clone()));
            }
        }
    }
    labels
}

/// T3: the guard denies every command the CLI refuses to anyone but the
/// owner, so an agent learns at the hook, not from a failed call. The CLI
/// remains the boundary; this keeps the two lists from drifting apart.
#[test]
fn guard_denies_every_cli_owner_command() {
    let labels = cli_owner_labels();
    assert!(labels.len() >= 15, "found only {labels:?}");
    for (label, file) in &labels {
        // `mem.rs` names its command without the program: "mem approve".
        let command = if label.starts_with("tiller ") {
            label.clone()
        } else {
            format!("tiller {label}")
        };
        let command = format!("{command} 01JABC");
        for role in Role::ALL {
            match bash(role, &command) {
                Some(denial) => assert!(
                    [Code::OwnerOnly, Code::Peer].contains(&denial.code),
                    "{file}: {role}: {command}: {}",
                    denial.message()
                ),
                None => panic!("{file}: {role}: {command} is the owner's, but the guard allows it"),
            }
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
        "tiller peer accept --from host-a --viewer-host-id 01JABC --pubkey abc --scope act --unattended --json",
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
        "ssh -T -o BatchMode=yes -- host-b tiller peer accept --from host-a --unattended --json",
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

// -- Wrapper option parsing ---------------------------------------------------

/// Runs each case for every role: `None` is an allow.
#[track_caller]
fn verdicts(cases: &[(&str, Option<Code>)]) {
    for role in Role::ALL {
        for (command, verdict) in cases {
            match verdict {
                Some(code) => denied(role, command, *code),
                None => allowed(role, command),
            }
        }
    }
}

/// B2: `env` reads its options the way GNU `getopt_long` does.
#[test]
fn env_options_parse_like_getopt() {
    const ID: Option<Code> = Some(Code::Identity);
    verdicts(&[
        // Bundled short options.
        ("env -iu FOO tiller status x", ID),
        ("env -vi tiller status x", ID),
        ("env -0i tiller status x", ID),
        // Attached and detached values.
        ("env -uTILLER_SESSION_ID tiller status x", ID),
        ("env -vuCLAUDECODE tiller status x", ID),
        ("env --unset CLAUDECODE tiller status x", ID),
        ("env -C /home/user -i tiller status x", ID),
        ("env -C/home/user -i tiller status x", ID),
        ("env -a name -i tiller status x", ID),
        // Long options and their unique prefixes.
        ("env --ignore-environment tiller status x", ID),
        ("env --ignore-env tiller status x", ID),
        ("env --uns=CLAUDECODE tiller status x", ID),
        // `-` after the options, also after `--`.
        ("env - tiller status x", ID),
        ("env -- - tiller status x", ID),
        // A name the guard cannot read.
        ("env -u \"$NAME\" tiller status x", ID),
        // `-S` splits its value into more `env` arguments.
        ("env -S '-i tiller status x'", ID),
        ("env -S'-i' tiller status x", ID),
        ("env -vS '-u CLAUDECODE' tiller status x", ID),
        ("env --split-string='-i' tiller status x", ID),
        ("env -S '-C /home/user -i' tiller status x", ID),
        ("env -S 'gh pr merge 12'", Some(Code::Merge)),
        ("env -S'gh pr' merge 12", Some(Code::Merge)),
        ("env -S 'FOO=1 gh pr merge 12'", Some(Code::Merge)),
        // Assignments after the options, also after `--`.
        ("env -- PATH=./bin tiller status x", Some(Code::GuardTamper)),
        ("env -i0 -- gh pr merge 12", ID),
        ("env -- gh pr merge 12", Some(Code::Merge)),
        ("env -C /home/user gh pr merge 12", Some(Code::Merge)),
        // An option env does not take: fail closed on a protected command.
        ("env --bogus gh pr merge 12", Some(Code::Merge)),
        ("env FOO=1 cargo test", None),
        ("env -u FOO cargo test", None),
        ("env -uFOO -C /home/user/pool/3/alpha cargo test", None),
        ("env -S 'cargo test --workspace'", None),
        ("env -- cargo build", None),
        ("env --bogus cargo test", None),
    ]);
}

/// B2: the `exec` builtin reads `-c`, `-l` and `-a NAME` in any cluster.
#[test]
fn exec_options_parse_like_the_builtin() {
    const ID: Option<Code> = Some(Code::Identity);
    verdicts(&[
        ("exec -c tiller status x", ID),
        ("exec -cl tiller status x", ID),
        ("exec -lc tiller status x", ID),
        ("exec -a name -c tiller status x", ID),
        ("exec -ca name tiller status x", ID),
        ("exec -la name gh pr merge 12", Some(Code::Merge)),
        ("exec -aname gh pr merge 12", Some(Code::Merge)),
        ("exec -a gh -- gh pr merge 12", Some(Code::Merge)),
        ("exec cargo test", None),
        ("exec -a name cargo build", None),
        ("exec -l cargo build", None),
    ]);
}

/// B3: programs that run their remaining words as a command are unwrapped
/// after their options and operands, so the inner command gets its checks.
#[test]
fn command_runners_unwrap_after_their_options() {
    const MERGE: Option<Code> = Some(Code::Merge);
    verdicts(&[
        ("strace -f gh pr merge 12", MERGE),
        ("strace -o /tmp/t gh pr merge 12", MERGE),
        ("strace -otrace gh pr merge 12", MERGE),
        ("strace -fo trace gh pr merge 12", MERGE),
        ("strace -e trace=network gh pr merge 12", MERGE),
        ("strace --output=trace gh pr merge 12", MERGE),
        ("strace --output trace gh pr merge 12", MERGE),
        (
            "strace -E TILLER_SESSION_ID tiller status x",
            Some(Code::Identity),
        ),
        (
            "strace -E PATH=./bin tiller status x",
            Some(Code::GuardTamper),
        ),
        ("ltrace -o trace gh pr merge 12", MERGE),
        ("ltrace -S -s 80 gh pr merge 12", MERGE),
        ("nice gh pr merge 12", MERGE),
        ("nice -n 5 gh pr merge 12", MERGE),
        ("nice -n5 gh pr merge 12", MERGE),
        ("nice --adjustment=5 gh pr merge 12", MERGE),
        ("nice --adj 5 gh pr merge 12", MERGE),
        ("nice -5 gh pr merge 12", MERGE),
        ("nice --10 gh pr merge 12", MERGE),
        ("ionice -c 3 gh pr merge 12", MERGE),
        ("ionice -c3 -n7 -t gh pr merge 12", MERGE),
        ("timeout 30 gh pr merge 12", MERGE),
        ("timeout -s KILL 30 gh pr merge 12", MERGE),
        ("timeout -sKILL -k 5 30 gh pr merge 12", MERGE),
        ("timeout --signal=KILL 30 gh pr merge 12", MERGE),
        ("timeout --kill-after 5 30 gh pr merge 12", MERGE),
        ("timeout -fv 30 gh pr merge 12", MERGE),
        ("timeout --foreground -- 30 gh pr merge 12", MERGE),
        ("nohup gh pr merge 12", MERGE),
        ("nohup -- gh pr merge 12", MERGE),
        ("chrt 5 gh pr merge 12", MERGE),
        ("chrt -f 5 gh pr merge 12", MERGE),
        ("chrt -T 1000 -d 0 gh pr merge 12", MERGE),
        (
            "chrt --sched-runtime=1000 --deadline 0 gh pr merge 12",
            MERGE,
        ),
        ("chrt -o gh pr merge 12", MERGE),
        ("taskset 0x3 gh pr merge 12", MERGE),
        ("taskset -c 0-2 gh pr merge 12", MERGE),
        ("taskset -ac 1 gh pr merge 12", MERGE),
        ("setsid gh pr merge 12", MERGE),
        ("setsid -cw gh pr merge 12", MERGE),
        ("setsid --fork gh pr merge 12", MERGE),
        ("stdbuf -oL gh pr merge 12", MERGE),
        ("stdbuf -o L gh pr merge 12", MERGE),
        ("stdbuf --output=L -e0 gh pr merge 12", MERGE),
        ("unshare -r gh pr merge 12", MERGE),
        ("unshare -Ur gh pr merge 12", MERGE),
        ("unshare --map-user 0 gh pr merge 12", MERGE),
        ("unshare -R /home/user gh pr merge 12", MERGE),
        ("unshare --mount=/home/user/ns -m gh pr merge 12", MERGE),
        ("nsenter -t 1 -m gh pr merge 12", MERGE),
        ("nsenter -t1 -m/proc/1/ns/mnt gh pr merge 12", MERGE),
        ("nsenter --target 1 --mount gh pr merge 12", MERGE),
        ("setpriv --ruid 0 gh pr merge 12", MERGE),
        ("setpriv --nnp -d gh pr merge 12", MERGE),
        ("setpriv --reset-env tiller status x", Some(Code::Identity)),
        ("runuser -u nobody gh pr merge 12", MERGE),
        ("runuser -unobody -- gh pr merge 12", MERGE),
        ("runuser nobody -c 'gh pr merge 12'", MERGE),
        ("runuser -c 'gh pr merge 12' nobody", MERGE),
        ("runuser --command='gh pr merge 12' nobody", MERGE),
        (
            "runuser -l nobody -c 'tiller status x'",
            Some(Code::Identity),
        ),
        ("runuser - nobody", Some(Code::Identity)),
        ("runuser -P -u nobody tiller status x", Some(Code::Identity)),
        ("chroot / gh pr merge 12", MERGE),
        ("chroot --userspec=1:1 / gh pr merge 12", MERGE),
        ("chroot --userspec 1:1 --skip-chdir / gh pr merge 12", MERGE),
        ("fakeroot gh pr merge 12", MERGE),
        ("fakeroot -u gh pr merge 12", MERGE),
        ("fakeroot -l libfakeroot.so gh pr merge 12", MERGE),
        ("fakeroot --lib libfakeroot.so -- gh pr merge 12", MERGE),
        ("flock -w 5 /tmp/l gh pr merge 12", MERGE),
        ("flock -w5 /tmp/l gh pr merge 12", MERGE),
        ("flock -xn /tmp/l gh pr merge 12", MERGE),
        ("flock --timeout=5 /tmp/l gh pr merge 12", MERGE),
        ("flock /tmp/l --command 'gh pr merge 12'", MERGE),
        ("/usr/bin/time -f %e gh pr merge 12", MERGE),
        ("/usr/bin/time -o out -a gh pr merge 12", MERGE),
        ("/usr/bin/time --format=%e gh pr merge 12", MERGE),
        ("caffeinate -i gh pr merge 12", MERGE),
        ("caffeinate -t 60 gh pr merge 12", MERGE),
        ("caffeinate -dimsu -w 123 gh pr merge 12", MERGE),
        ("systemd-inhibit gh pr merge 12", MERGE),
        ("systemd-inhibit --what=idle gh pr merge 12", MERGE),
        ("systemd-inhibit --why x --no-pager gh pr merge 12", MERGE),
        // Runners nest.
        (
            "nice -n 5 timeout -s INT 30 strace -f gh pr merge 12",
            MERGE,
        ),
        // An option the runner does not take: fail closed on a protected
        // command.
        ("timeout --bogus 30 gh pr merge 12", MERGE),
        ("timeout \"$OPTS\" 30 gh pr merge 12", MERGE),
        // The forms that run no command.
        ("ionice -p 1234", None),
        ("chrt -p 1234", None),
        ("chrt -m", None),
        ("taskset -p 1234", None),
        ("taskset -cp 1 1234", None),
        ("flock 9", None),
        ("strace -f ls", None),
        ("nice -n 5 cargo build", None),
        ("nice -5 cargo build", None),
        ("timeout 30 cargo nextest run", None),
        ("timeout -s INT 30 cargo test", None),
        ("timeout --bogus 30 cargo test", None),
        ("timeout \"$SECONDS\" cargo test", None),
        ("stdbuf -oL cargo test", None),
        ("taskset -c 0-3 cargo build", None),
        ("setsid -f cargo build", None),
        ("flock -w 5 /tmp/l cargo build", None),
        ("/usr/bin/time -v cargo build", None),
        ("caffeinate -i cargo build", None),
    ]);
}

/// B3: `tcsh`, `csh` and `pwsh` run a command string, like `sh -c`.
#[test]
fn more_shells_run_their_command_string() {
    const MERGE: Option<Code> = Some(Code::Merge);
    verdicts(&[
        ("tcsh -c 'gh pr merge 12'", MERGE),
        ("csh -fc 'gh pr merge 12'", MERGE),
        ("echo 'gh pr merge 12' | tcsh", MERGE),
        ("pwsh -c 'gh pr merge 12'", MERGE),
        ("pwsh -NoProfile -Command gh pr merge 12", MERGE),
        ("pwsh -command 'gh pr merge 12'", MERGE),
        ("pwsh --command 'gh pr merge 12'", MERGE),
        ("pwsh -ExecutionPolicy Bypass -c 'gh pr merge 12'", MERGE),
        (
            "pwsh -EncodedCommand ZwBoACAAcAByACAAbQBlAHIAZwBlACAAMQAyAA==",
            MERGE,
        ),
        (
            "pwsh -nop -ec ZwBoACAAcAByACAAbQBlAHIAZwBlACAAMQAyAA==",
            MERGE,
        ),
        ("echo 'gh pr merge 12' | pwsh -Command -", MERGE),
        ("tcsh -c 'echo hi'", None),
        ("pwsh -c 'Get-ChildItem'", None),
        ("pwsh -File build.ps1", None),
        ("pwsh build.ps1", None),
    ]);
}

/// A word the guard cannot read where a runner reads its options may be an
/// operand or an option: every reading gets the inner command's checks.
#[test]
fn runner_variable_operand_keeps_the_inner_checks() {
    const MERGE: Option<Code> = Some(Code::Merge);
    const ID: Option<Code> = Some(Code::Identity);
    verdicts(&[
        ("timeout \"$T\" env -i true", ID),
        ("flock \"$L\" env -u TILLER_SESSION_ID true", ID),
        ("chroot \"$DIR\" gh pr merge 12", MERGE),
        ("taskset \"$MASK\" gh pr merge 12", MERGE),
        ("timeout $T gh pr merge 12", MERGE),
        ("timeout \"$OPTS\" 30 gh pr merge 12", MERGE),
        ("timeout \"$A\" \"$B\" 30 gh pr merge 12", MERGE),
        ("timeout -s \"$SIG\" \"$T\" env -i true", ID),
        ("timeout --signal=\"$SIG\" 30 gh pr merge 12", MERGE),
        ("timeout -$X 30 gh pr merge 12", MERGE),
        ("setpriv \"$X\" tiller status x", Some(Code::OwnerOnly)),
        ("timeout \"$T\" git status", None),
        ("timeout \"$T\" cargo test", None),
        ("flock \"$LOCK\" git fetch", None),
        ("taskset \"$MASK\" gh pr view 1", None),
        ("timeout --signal=\"$SIG\" 30 git status", None),
    ]);
}

/// A runner's output file is a write, and a strace output that starts with
/// `|` or `!` is a command strace runs.
#[test]
fn runner_output_files_are_writes() {
    const STATE: Option<Code> = Some(Code::StateWrite);
    verdicts(&[
        ("strace -o /home/user/.tiller/state.db true", STATE),
        ("strace -o/home/user/.tiller/state.db true", STATE),
        ("strace --output=/home/user/.tiller/state.db true", STATE),
        ("ltrace -o /home/user/.tiller/state.db true", STATE),
        ("/usr/bin/time -o /home/user/.tiller/state.db true", STATE),
        ("flock /home/user/.tiller/state.db true", STATE),
        ("flock -w 5 /home/user/.tiller/state.db -c true", STATE),
        ("strace -o '|gh pr merge 12' true", Some(Code::Merge)),
        ("strace -o '!gh pr merge 12' true", Some(Code::Merge)),
        ("strace -fo'|gh pr merge 12' true", Some(Code::Merge)),
        ("strace -o /tmp/trace cargo test", None),
        ("strace -o '|grep open' cargo test", None),
        ("/usr/bin/time -o /tmp/time.txt cargo build", None),
        ("flock /tmp/l cargo build", None),
        ("flock 9", None),
    ]);
}

/// BSD `time` (macOS `/usr/bin/time`) takes `-l` and `-h`.
#[test]
fn bsd_time_options_are_read() {
    verdicts(&[
        ("/usr/bin/time -l git status", None),
        ("/usr/bin/time -lh cargo build", None),
        ("/usr/bin/time -l gh pr merge 12", Some(Code::Merge)),
    ]);
}

/// `pwsh` parameters: a `:` value stays in its word, `-CommandWithArgs` is
/// a command, an unknown parameter fails closed, and an encoded command the
/// guard cannot decode is a deny.
#[test]
fn pwsh_parameters_fail_closed() {
    const MERGE: Option<Code> = Some(Code::Merge);
    verdicts(&[
        ("pwsh -ep:Bypass -c 'gh pr merge 12'", MERGE),
        (
            "pwsh -ExecutionPolicy:Bypass -NoProfile -c 'gh pr merge 12'",
            MERGE,
        ),
        ("pwsh -cwa 'gh pr merge 12'", MERGE),
        ("pwsh -CommandWithArgs 'gh pr merge 12' a b", MERGE),
        ("pwsh -Bogus -c 'gh pr merge 12'", MERGE),
        ("echo 'gh pr merge 12' | pwsh -File -", MERGE),
        (
            "pwsh -ec 'ZwBoACAA cAByACAAbQBlAHIAZwBlACAAMQAyAA=='",
            MERGE,
        ),
        ("pwsh -ec \"$B\"", Some(Code::GuardTamper)),
        ("pwsh -ec 'not base64!'", Some(Code::GuardTamper)),
        ("echo x | xargs pwsh -ec", Some(Code::GuardTamper)),
        ("pwsh -ec", None),
        ("pwsh -ep:Bypass -c 'Get-ChildItem'", None),
        ("pwsh -NoLogo -NonInteractive -c 'Get-ChildItem'", None),
        ("pwsh -wd /home/user/pool/3/alpha -File build.ps1", None),
        ("pwsh -ec RwBlAHQALQBDAGgAaQBsAGQASQB0AGUAbQA=", None),
    ]);
}

/// A runner with no command starts a shell, and that shell reads the
/// script on standard input, as bare `sh` does.
#[test]
fn runners_with_no_command_start_a_shell() {
    const MERGE: Option<Code> = Some(Code::Merge);
    verdicts(&[
        ("echo 'gh pr merge 12' | fakeroot", MERGE),
        ("echo 'gh pr merge 12' | unshare -r", MERGE),
        ("echo 'gh pr merge 12' | chroot /", MERGE),
        ("echo 'gh pr merge 12' | nsenter -t 1 -m", MERGE),
        ("echo 'gh pr merge 12' | runuser user", MERGE),
        ("echo 'gh pr merge 12' | sudo -s", MERGE),
        ("echo 'gh pr merge 12' | sudo -i", MERGE),
        ("echo 'gh pr merge 12' | doas -s", MERGE),
        ("echo 'gh pr merge 12' | su", MERGE),
        ("echo 'gh pr merge 12' | su user", MERGE),
        ("fakeroot < /home/user/merge.sh", None),
        ("echo hi | fakeroot", None),
        ("unshare -r", None),
        ("chroot /", None),
    ]);
}

/// util-linux `su` and `runuser` read their options anywhere before `--`,
/// also after the user.
#[test]
fn runuser_reads_options_after_the_user() {
    const ID: Option<Code> = Some(Code::Identity);
    verdicts(&[
        ("runuser user -l -c true", ID),
        ("runuser user -P -c true", ID),
        ("runuser user --login -c true", ID),
        ("runuser user -c 'gh pr merge 12' -l", ID),
        (
            "runuser user -s /bin/sh -c 'gh pr merge 12'",
            Some(Code::Merge),
        ),
        ("runuser user -- -c 'gh pr merge 12'", Some(Code::Merge)),
        ("runuser user -c true", None),
        ("runuser user -m -c 'cargo test'", None),
    ]);
}

/// `sudo`, `doas` and `su` read their options like getopt: bundles,
/// attached values and long options.
#[test]
fn sudo_and_su_options_parse_like_getopt() {
    const MERGE: Option<Code> = Some(Code::Merge);
    verdicts(&[
        ("sudo -iu root gh pr merge 12", MERGE),
        ("sudo -Eu root gh pr merge 12", MERGE),
        ("sudo -uroot gh pr merge 12", MERGE),
        ("sudo --user=root gh pr merge 12", MERGE),
        ("sudo --preserve-env=PATH -u root gh pr merge 12", MERGE),
        ("sudo -u root -- gh pr merge 12", MERGE),
        ("sudo -u root FOO=1 gh pr merge 12", MERGE),
        ("sudo PATH=/tmp tiller status x", Some(Code::GuardTamper)),
        ("sudo --bogus gh pr merge 12", MERGE),
        (
            "sudo -e /home/user/.tiller/state.db",
            Some(Code::StateWrite),
        ),
        ("doas -u root gh pr merge 12", MERGE),
        ("doas -nu root gh pr merge 12", MERGE),
        ("su -lc 'gh pr merge 12'", Some(Code::Identity)),
        ("su - user -c true", Some(Code::Identity)),
        ("su -c'gh pr merge 12'", MERGE),
        ("su --command='gh pr merge 12'", MERGE),
        ("su --command 'gh pr merge 12' user", MERGE),
        ("su user -c 'gh pr merge 12'", MERGE),
        ("sudo -u root cargo test", None),
        ("sudo -iu root ls", None),
        ("sudo -k true", None),
        ("doas -u root ls", None),
        ("su -c 'echo hi'", None),
        ("su -c'cargo test' user", None),
    ]);
}

/// `xargs` and the shells read their long options from a full table.
#[test]
fn xargs_and_shell_long_options_are_read() {
    const MERGE: Option<Code> = Some(Code::Merge);
    verdicts(&[
        ("xargs --max-chars 100 gh pr merge 12", MERGE),
        ("xargs --process-slot-var V gh pr merge 12", MERGE),
        ("xargs -s100 gh pr merge 12", MERGE),
        ("xargs -0rn1 gh pr merge 12", MERGE),
        ("xargs --bogus gh pr merge 12", MERGE),
        ("echo merge | xargs -I@ gh pr @ 12", MERGE),
        ("echo merge | xargs -J @ gh pr @ 12", MERGE),
        ("fish --command 'gh pr merge 12'", MERGE),
        ("fish --command='gh pr merge 12'", MERGE),
        ("fish -C 'gh pr merge 12'", MERGE),
        ("fish --init-command='gh pr merge 12' -c true", MERGE),
        ("bash --bogus gh pr merge 12", MERGE),
        ("bash --norc --noprofile -c 'gh pr merge 12'", MERGE),
        ("bash --rcfile /home/user/rc -c 'gh pr merge 12'", MERGE),
        ("git ls-files -z | xargs -0 --max-args=20 cat", None),
        ("xargs --max-chars 100 cargo test", None),
        ("fish --command 'echo hi'", None),
        ("bash --norc -c 'cargo test'", None),
        ("bash -C script.sh", None),
    ]);
}

/// Every verdict that does not match, so a test names each form that fails
/// instead of stopping at the first.
fn mismatches(cases: &[(&str, Option<Code>)]) -> Vec<String> {
    let mut wrong = Vec::new();
    for role in Role::ALL {
        for (command, verdict) in cases {
            let got = bash(role, command).map(|denial| denial.code);
            if got != *verdict {
                wrong.push(format!(
                    "{role}: {command}: got {got:?}, wanted {verdict:?}"
                ));
            }
        }
    }
    wrong
}

/// `watch` reads its options the way procps `watch` does, and runs the
/// rest as one `sh -c` string unless `-x` is among them.
#[test]
fn watch_options_parse_like_getopt() {
    const OWNER: Option<Code> = Some(Code::OwnerOnly);
    let wrong = mismatches(&[
        ("watch -n 5 tiller approve 01JABC", OWNER),
        ("watch -n5 tiller approve 01JABC", OWNER),
        ("watch -d -n 5 tiller approve 01JABC", OWNER),
        ("watch -td -n 5 tiller approve 01JABC", OWNER),
        // `-d` takes an attached value: `-dn` is `-d n`, so watch runs `5`.
        ("watch -dn 5 tiller approve 01JABC", None),
        ("watch -dn5 tiller approve 01JABC", OWNER),
        ("watch -tbn 5 tiller approve 01JABC", OWNER),
        ("watch --interval=5 tiller approve 01JABC", OWNER),
        ("watch --interval 5 tiller approve 01JABC", OWNER),
        ("watch --inter 5 tiller approve 01JABC", OWNER),
        ("watch --differences=permanent tiller approve 01JABC", OWNER),
        ("watch -dpermanent tiller approve 01JABC", OWNER),
        ("watch -q 3 tiller approve 01JABC", OWNER),
        ("watch --equexit=3 tiller approve 01JABC", OWNER),
        ("watch -s /home/user/shots tiller approve 01JABC", OWNER),
        (
            "watch --shotsdir /home/user/shots tiller approve 01JABC",
            OWNER,
        ),
        ("watch -- tiller approve 01JABC", OWNER),
        ("watch -n 5 -- tiller approve 01JABC", OWNER),
        // `-x` in any cluster, before or after the other options.
        ("watch -x tiller approve 01JABC", OWNER),
        ("watch -xn 5 tiller approve 01JABC", OWNER),
        ("watch -n 5 -x tiller approve 01JABC", OWNER),
        ("watch --exec --interval=5 tiller approve 01JABC", OWNER),
        ("watch -x sh -c 'tiller approve 01JABC'", OWNER),
        // Without `-x` the words are one command string.
        ("watch 'tiller approve 01JABC'", OWNER),
        ("watch -n 5 'tiller approve' 01JABC", OWNER),
        ("watch -dn 5 'true; tiller approve 01JABC'", OWNER),
        // An option watch does not take, or one not known before it runs.
        ("watch --bogus tiller approve 01JABC", OWNER),
        ("watch -n 5 \"$OPT\" tiller approve 01JABC", OWNER),
        ("watch -tn 5 gh pr merge 12", Some(Code::Merge)),
        ("watch -dn 5 ls -la", None),
        ("watch -x -n 2 cargo test", None),
        ("watch -n 1 'tiller --json snapshot'", None),
        ("watch --bogus cargo test", None),
    ]);
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// `nix develop` and `nix shell` run the words after `-c` or `--command`,
/// in any cluster and after any flag, and a shell on standard input with no
/// command. `nix-shell` runs its last `--run` or `--command` string.
#[test]
fn nix_command_forms_are_read() {
    const OWNER: Option<Code> = Some(Code::OwnerOnly);
    const ID: Option<Code> = Some(Code::Identity);
    let wrong = mismatches(&[
        ("nix develop -c tiller approve 01JABC", OWNER),
        ("nix develop -L -c tiller approve 01JABC", OWNER),
        ("nix develop -Lc tiller approve 01JABC", OWNER),
        ("nix develop -vLc tiller approve 01JABC", OWNER),
        ("nix shell nixpkgs#hello -c tiller approve 01JABC", OWNER),
        (
            "nix shell nixpkgs#hello --command tiller approve 01JABC",
            OWNER,
        ),
        // Flags before the subcommand.
        ("nix -L develop -c tiller approve 01JABC", OWNER),
        (
            "nix --extra-experimental-features 'nix-command flakes' develop -c tiller approve 01JABC",
            OWNER,
        ),
        (
            "nix --option sandbox false shell nixpkgs#hello -c tiller approve 01JABC",
            OWNER,
        ),
        (
            "nix env shell nixpkgs#hello -c tiller approve 01JABC",
            OWNER,
        ),
        // A flag value that looks like `-c`.
        (
            "nix develop --keep-env-var -c --command tiller approve 01JABC",
            OWNER,
        ),
        ("nix develop --arg x -c -c tiller approve 01JABC", OWNER),
        // nix splits `-c/path` into `-c` and `/path`.
        (
            "nix develop -c/nix/store/abc-tiller-0.1.0/bin/tiller approve 01JABC",
            OWNER,
        ),
        // A word not known before it runs may be `-c`.
        ("nix develop \"$OPT\" tiller approve 01JABC", OWNER),
        // No command: a shell reads standard input.
        ("echo 'tiller approve 01JABC' | nix develop", OWNER),
        (
            "echo 'tiller approve 01JABC' | nix shell nixpkgs#hello",
            OWNER,
        ),
        ("nix develop <<< 'tiller approve 01JABC'", OWNER),
        // `-i` clears the environment.
        ("nix develop -i -c tiller status x", ID),
        ("nix develop -Lic tiller status x", ID),
        ("nix shell nixpkgs#hello --ignore-env -c true", ID),
        ("nix-shell --run 'tiller approve 01JABC'", OWNER),
        (
            "nix-shell -p hello --command 'tiller approve 01JABC'",
            OWNER,
        ),
        // The last string wins, and a flag value may look like `--run`.
        ("nix-shell --run true --run 'tiller approve 01JABC'", OWNER),
        (
            "nix-shell --arg x --run --run 'tiller approve 01JABC'",
            OWNER,
        ),
        ("nix-shell \"$OPT\" 'tiller approve 01JABC'", OWNER),
        ("echo 'tiller approve 01JABC' | nix-shell -p hello", OWNER),
        ("nix-shell --pure --run true", ID),
        (
            "nix develop -c cargo clippy --workspace -- -D warnings",
            None,
        ),
        ("nix develop -Lc cargo test", None),
        ("nix shell nixpkgs#jq -c jq . data.json", None),
        ("nix-shell -p jq --run 'jq . data.json'", None),
        ("nix develop", None),
        ("nix build .#tiller", None),
        ("nix flake check -L", None),
        ("echo hi | nix develop -c cat", None),
    ]);
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// `direnv exec DIR COMMAND`, or `direnv exec FILE` when the path is a
/// file: the command may start at either word.
#[test]
fn direnv_exec_reads_both_shapes() {
    const OWNER: Option<Code> = Some(Code::OwnerOnly);
    let wrong = mismatches(&[
        ("direnv exec . tiller approve 01JABC", OWNER),
        (
            "direnv exec /home/user/pool/3/alpha tiller approve 01JABC",
            OWNER,
        ),
        (
            "direnv exec /nix/store/abc-tiller-0.1.0/bin/tiller approve 01JABC",
            OWNER,
        ),
        ("direnv exec \"$DIR\" tiller approve 01JABC", OWNER),
        ("direnv \"$SUB\" . tiller approve 01JABC", OWNER),
        ("direnv exec . sh -c 'tiller approve 01JABC'", OWNER),
        ("direnv exec . cargo test", None),
        ("direnv exec /home/user/pool/3/alpha tiller status x", None),
        ("direnv allow", None),
    ]);
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// A brace word is not known before it runs: past the positional, it can
/// only be a flag.
#[test]
fn gh_brace_word_is_unread() {
    verdicts(&[
        ("gh pr close 5 {-d,}", Some(Code::Merge)),
        ("gh pr close 5 --{delete-branch,}", Some(Code::Merge)),
        ("gh pr close 5 -c {done,}", None),
    ]);
}

/// A brace sequence makes several words as a brace list does, and a quoted
/// brace is literal text.
#[test]
fn brace_sequences_are_unread_and_quoted_braces_are_text() {
    verdicts(&[
        ("gh pr close 5 -{d..d}", Some(Code::Merge)),
        ("gh pr close 5 \"$HOME{-d,}\"", None),
        ("mkdir -p src/{1..3}", None),
    ]);
}

/// A shell reads `-` as the end of its options, and a word it cannot read
/// where its options are may be `-c`.
#[test]
fn shell_dash_and_unread_options_fail_closed() {
    const MERGE: Option<Code> = Some(Code::Merge);
    verdicts(&[
        ("echo 'gh pr merge 12' | bash -", MERGE),
        ("echo 'gh pr merge 12' | sh -", MERGE),
        ("echo 'gh pr merge 12' | su root -s /bin/sh -", MERGE),
        ("bash -$O 'gh pr merge 12'", MERGE),
        ("bash $O 'gh pr merge 12'", MERGE),
        ("su root -- -$O 'gh pr merge 12'", MERGE),
        ("bash - ./build.sh", None),
        ("bash \"$SCRIPT\"", None),
        ("bash -$O ./build.sh", None),
    ]);
}

/// A guessed reading of a runner's options that the program would reject
/// adds no place for the command to start, and the guesses are capped.
#[test]
fn runner_guesses_drop_rejected_readings_and_stop() {
    verdicts(&[
        ("timeout $T git -C repo status", None),
        // `nice` takes no operand, so `$N` may be the program, as with
        // `$CMD git status`.
        ("nice $N git -C repo status", Some(Code::GuardTamper)),
    ]);
    let nested = format!("{}gh pr merge 12", "nice $X ".repeat(30));
    let started = std::time::Instant::now();
    for role in Role::ALL {
        assert!(bash(role, &nested).is_some(), "{role}: {nested}: allowed");
    }
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
}

/// `sudo -l COMMAND` and the other listing options run no command, and
/// `doas -a STYLE` is read.
#[test]
fn sudo_listing_runs_no_command() {
    verdicts(&[
        ("sudo -l gh pr merge 12", None),
        ("sudo -V", None),
        ("doas -C /etc/doas.conf gh pr merge 12", None),
        ("doas -a passwd gh pr view 1", None),
        ("doas -a passwd gh pr merge 12", Some(Code::Merge)),
        ("sudo -k gh pr merge 12", Some(Code::Merge)),
        ("sudo -hexample.com gh pr merge 12", Some(Code::Merge)),
    ]);
}

/// Owner decision, Wednesday 2026-11-11: a word the guard cannot read in
/// the PR number slot is the number; one after it can only be a flag, so it
/// is read as the flag the rule checks for.
#[test]
fn gh_unreadable_word_after_the_positional_is_the_flag() {
    verdicts(&[
        ("gh pr close 5 $FLAG", Some(Code::Merge)),
        ("gh pr close $PR $FLAG", Some(Code::Merge)),
        ("gh pr close $FLAG 5", Some(Code::Merge)),
        ("gh pr close 5 \"$(printf -- -d)\"", Some(Code::Merge)),
        ("gh pr close 5 -$LETTER", Some(Code::Merge)),
        ("gh pr close 5 --delete-$REST", Some(Code::Merge)),
        ("gh-axi pr close 5 $FLAG", Some(Code::Merge)),
        ("gh repo edit example/alpha $FLAG", Some(Code::DefaultPush)),
        ("gh pr close $PR", None),
        ("gh pr close \"$PR\" -c done", None),
        ("gh pr close 5 -c \"$MSG\"", None),
        ("gh pr close 5 -c$MSG", None),
        ("gh pr close 5 --comment \"$MSG\"", None),
        ("gh pr close 5 --comment=\"$MSG\"", None),
        ("gh pr close -R \"$REPO\" $PR", None),
        ("gh repo edit $REPO --description \"$TEXT\"", None),
        ("gh repo edit --homepage \"$URL\"", None),
    ]);
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
