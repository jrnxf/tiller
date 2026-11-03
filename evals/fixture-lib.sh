# shellcheck shell=bash
# Helpers for a case's fixture.sh. scaffold.sh puts them in each scaffold,
# after the stub tiller is installed and before the fixture runs, in the
# run's empty workspace.

# reply KEY [EXIT] - what the stub prints for `tiller <first> <second>`, where
# KEY is "<first>-<second>", read from stdin. EXIT makes the call fail with
# that code, the way a refused action does.
reply() {
	cat >".tiller/replies/$1"
	[ -z "${2:-}" ] || printf '%s\n' "$2" >".tiller/replies/$1.exit"
}

# repo - a git repository on main with a local bare origin and a scratch
# directory. The stub's files, the origin and the scratch stay out of git.
repo() {
	git init -q -b main .
	git config user.name 'Eval Author'
	git config user.email author@example.com
	git config commit.gpgsign false
	printf '%s\n' .tiller/ .remote/ .scratch/ >>.git/info/exclude
	git init -q --bare -b main .remote/origin.git
	git remote add origin "$PWD/.remote/origin.git"
	mkdir -p .scratch
}

# commit MESSAGE - commit everything in the workspace.
commit() { git add -A && git commit -q -m "$1"; }
