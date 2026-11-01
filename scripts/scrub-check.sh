#!/usr/bin/env bash
# Fail when a tracked file could leak private information: home paths,
# LAN hostnames and addresses, email addresses, machine names, or the names
# of the owner's private projects.
#
# Private names are kept as SHA-256 hashes in scripts/scrub-terms.sha256, so
# this public repo never spells them out. Each is one token (letters, digits,
# ".", "_" or "-"), matched whole or as a part split on those marks. Add one with:
#   printf %s 'name' | tr '[:upper:]' '[:lower:]' | sha256sum
# TILLER_SCRUB_TERMS may name a local plain-text file (one term per line,
# kept outside the repo) with extra terms to check; a term with a space or
# other mark there is matched as a case-insensitive substring.
#
# Usage: scripts/scrub-check.sh
set -euo pipefail

root=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
terms="$root/scripts/scrub-terms.sha256"

git -C "$root" ls-files -z | perl -e '
use strict;
use warnings;
use Digest::SHA qw(sha256_hex);

my ($root, $terms, $extra) = @ARGV;
@ARGV = ();

my %hashed;
my @phrases;
open my $t, "<", $terms or die "scrub-check: cannot read $terms: $!\n";
while (my $line = <$t>) {
  $line =~ s/#.*//;
  $line =~ s/\s+//g;
  $hashed{lc $line} = 1 if length $line;
}
close $t;
if ($extra) {
  open my $e, "<", $extra or die "scrub-check: cannot read $extra: $!\n";
  while (my $line = <$e>) {
    $line =~ s/^\s+|\s+$//g;
    next if $line eq "" || $line =~ /^#/;
    if ($line =~ /[^A-Za-z0-9._-]/) {
      push @phrases, lc $line;
    } else {
      $hashed{sha256_hex(lc $line)} = 1;
    }
  }
  close $e;
}

my $ipv4 = qr/(?<![\d.])(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})(?![\d.]*\d)/;
sub private_ip {
  my @o = @_;
  return 0 if grep { $_ > 255 } @o;
  return 0 if $o[0] == 0 || $o[0] == 127;
  return 0 if "$o[0].$o[1].$o[2]" =~ /^(?:192\.0\.2|198\.51\.100|203\.0\.113)$/;
  return 0 if "@o" eq "192 168 1 20";
  return 1;
}

my @rules = (
  ["home path", qr{/(?:Users|home)/(?!user\b)[A-Za-z0-9._-]+|(?<![\w.])/(?:var/)?root/[A-Za-z0-9._-]+}],
  ["mounted volume", qr{/Volume[s]/}],
  ["home-relative path", qr{(?<![\w.~/-])~/(?!\.(?:tiller|claude|config|local|cache)\b)[A-Za-z0-9._-]+}],
  ["LAN link", qr{\blan\.[a-z0-9-]+\.[a-z]{2,}}i],
  ["LAN hostname", qr{(?<![.\w\$])[a-z0-9-]{2,}\.(?:lan|local|internal|home\.arpa|ts\.net)\b(?!-|\.?\w)}i],
  ["machine name", qr{\bagents["\x27`]? (?:host|machine|box)\b}i],
  ["email address", qr{\b[A-Za-z0-9._%+-]+@(?!example\.(?:com|org|net)\b)[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)*\.[A-Za-z]{2,}\b}],
);

sub private_terms {
  my ($text) = @_;
  my $n = 0;
  for my $token (split /[^A-Za-z0-9._-]+/, $text) {
    $token =~ s/^[._-]+|[._-]+$//g;
    next if $token eq "";
    for my $part ($token, split /[._-]+/, $token) {
      $n++ if $part ne "" && $hashed{sha256_hex(lc $part)};
    }
  }
  my $lower = lc $text;
  for my $phrase (@phrases) {
    $n++ if index($lower, $phrase) >= 0;
  }
  return $n;
}

my $found = 0;
local $/ = "\0";
while (my $path = <STDIN>) {
  chomp $path;
  if (private_terms($path)) {
    print "$path: private name in file path\n";
    $found++;
  }
  next if $path eq "scripts/scrub-terms.sha256";
  my $file = "$root/$path";
  next if -l $file || !-f $file || -B $file;
  open my $fh, "<", $file or die "scrub-check: cannot read $path: $!\n";
  local $/ = "\n";
  while (my $line = <$fh>) {
    for my $rule (@rules) {
      my ($name, $re) = @$rule;
      while ($line =~ /$re/g) {
        print "$path:$.: $name: $&\n";
        $found++;
      }
    }
    while ($line =~ /$ipv4/g) {
      next unless private_ip($1, $2, $3, $4);
      print "$path:$.: IP address: $&\n";
      $found++;
    }
    if (my $n = private_terms($line)) {
      print "$path:$.: private name ($n match", ($n == 1 ? "" : "es"), ", hidden)\n";
      $found++;
    }
  }
  close $fh;
}

if ($found) {
  print STDERR "scrub-check: $found finding(s)\n";
  exit 1;
}
print "scrub-check: clean\n";
' "$root" "$terms" "${TILLER_SCRUB_TERMS:-}"
