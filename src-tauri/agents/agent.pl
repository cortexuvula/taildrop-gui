# ---------------------------------------------------------------------------
# VENDORED FILE — the body below the marker is byte-identical to the source.
#
# Source: "omarchy-tailnet-files" (Two-Pane File Browser plugin for the
# Tailscale desktop), bin/tailfiles, the AGENT_PL raw string (lines
# 1018-1263), Copyright (c) the plugin authors, MIT License.
# Clone of record on this machine:
#   /Users/cortexuvula/.hermes/projects/taildrop-browser/omarchy-tailnet-files
# Full license text, provenance and the sha256 pin of the unmodified body:
#   src-tauri/agents/THIRD_PARTY.md
#
# Deviations from the plugin original (complete list):
#   1. This attribution header (Perl comments — semantically inert; the
#      length-prefixed framing below measures THIS file as sent, so the
#      header costs nothing on the wire beyond its bytes).
#   The functional body — every line from "use strict; use warnings;" to the
#   end of the file — is UNMODIFIED (byte-identical, sha256-pinned in
#   THIRD_PARTY.md). taildrop-gui drives the `recv` verb (atomic no-replace
#   placement + handle-bound stage cleanup); `send` and `rename` are vendored
#   unmodified for protocol completeness but are not invoked by this app
#   (source walks stay on SFTP; rename stays on its M1 tier).
#
# Invocation framing (plugin-exact, AGENT_CMD; see browser/agent.rs):
#   remote command = the FIXED string
#     perl -e 'my $n = <STDIN>; defined $n or exit 2; read(STDIN, my $c, $n)
#              == $n or exit 2; eval $c; die $@ if $@; exit 0'
#   stdin = "<byte-length of this file>\n" + this file + NUL-terminated
#   fields; the first field is the verb. Nothing user-controlled ever
#   reaches the remote shell (paths travel as stdin data — plugin invariant
#   §4.9, report.md §1.4).
# ---------------------------------------------------------------------------

use strict; use warnings;
use Fcntl qw(O_RDONLY O_WRONLY O_CREAT O_EXCL O_NOFOLLOW O_NONBLOCK :mode);
use Errno qw(EEXIST ENOENT EINVAL ENOSYS);
use POSIX ();
use Config;
binmode STDIN; binmode STDOUT;
$SIG{PIPE} = 'DEFAULT';

# A receive that doesn't finish removes the folder it made (relative to the
# destination folder it holds open; rm never follows a symlink).
my $UNDO;
sub bail { print STDERR $_[0], "\n"; drop_stage(@$UNDO) if $UNDO; exit($_[1] || 1) }
$SIG{HUP} = $SIG{TERM} = sub { bail("stopped", 130) };
sub field { local $/ = "\0"; my $f = <STDIN>; defined $f && chop($f) eq "\0" or bail("short input", 2); $f }
sub rec { print @_ }

# rename relative to two open folders; $excl refuses to replace anything.
my $NR;
if ($^O eq 'linux') {
    my $a = $Config{archname};
    $NR = $a =~ /^x86_64/ ? 316 : $a =~ /^(aarch64|riscv64|loongarch64)/ ? 276 : $a =~ /^arm/ ? 382
        : $a =~ /^i[3-6]86/ ? 353 : $a =~ /^(ppc64|powerpc64)/ ? 357 : $a =~ /^s390x/ ? 347 : undef;
}
sub rename_at {
    my ($sfd, $s, $dfd, $d, $excl) = @_;
    my $r;
    if ($^O eq 'darwin') { $r = syscall(488, $sfd + 0, "$s", $dfd + 0, "$d", $excl ? 4 : 0) }
    elsif (defined $NR) { $r = syscall($NR, $sfd + 0, "$s", $dfd + 0, "$d", $excl ? 1 : 0) }
    else { $! = ENOSYS; return 0 }
    return $r == 0;
}
sub unsafe_fs { my $e = $! + 0; $e == EINVAL || $e == ENOSYS }

sub fd_of { my $fd = fileno($_[0]); defined $fd or bail("perl here is too old (needs 5.22 or newer)", 9); $fd }

# Open folder $name inside the folder held by $ph, only if it is a real
# folder and the one that was checked (same device and inode).
sub open_dir_at {
    my ($ph, $name) = @_;
    chdir($ph) or return;
    my @l = lstat($name) or return;
    S_ISDIR($l[2]) or return;
    opendir(my $h, $name) or return;
    my @s = stat($h) or return;
    ($s[0] == $l[0] && $s[1] == $l[1]) or return;
    return $h;
}
sub names_in { my $h = shift; rewinddir($h); sort grep { $_ ne '.' && $_ ne '..' } readdir($h) }

# Remove everything inside the folder held by $h, through held handles only.
sub remove_at {
    my ($h, $depth) = @_;
    return if ($depth || 0) > 512;
    for my $n (names_in($h)) {
        chdir($h) or return;
        my @l = lstat($n) or next;
        if (S_ISDIR($l[2])) {
            my $c = open_dir_at($h, $n) or next;
            remove_at($c, ($depth || 0) + 1);
            closedir($c);
            chdir($h) or return;
            rmdir($n);
        } else {
            unlink($n);
        }
    }
}
# Empty the stage held by $S, then remove its name from $D only while that
# name still is this very folder.
sub drop_stage {
    my ($D, $name, $S) = @_;
    $UNDO = undef;
    remove_at($S);
    chdir($D) or return;
    my @l = lstat($name); my @s = stat($S);
    rmdir($name) if @l && @s && $l[0] == $s[0] && $l[1] == $s[1];
}
# The stage this copy just made: owned by this account, private, empty.
sub check_stage {
    my $S = shift;
    my @s = stat($S) or return 0;
    return $s[4] == $> && ($s[2] & 07777) == 0700 && !names_in($S);
}
sub open_top { my $p = shift; opendir(my $h, $p) or bail("Can't open $p: $!", 3); fd_of($h); $h }

# ---- send: stream the selected items, never following a symlink.
sub size_at {
    my ($ph, $name) = @_;
    chdir($ph) or return 0;
    my @l = lstat($name) or return 0;
    return $l[7] if S_ISREG($l[2]);
    return 0 unless S_ISDIR($l[2]);
    my $h = open_dir_at($ph, $name) or return 0;
    my $t = 0; $t += size_at($h, $_) for names_in($h);
    return $t;
}
sub send_at {
    my ($ph, $name, $rel) = @_;
    chdir($ph) or bail("lost the folder of $rel", 6);
    my @l = lstat($name);
    if (!@l) { bail("$rel disappeared while copying", 6) }
    if (S_ISLNK($l[2])) {
        my $t = readlink($name);
        defined $t ? rec("L", "$rel\0$t\0") : rec("S");
    } elsif (S_ISDIR($l[2])) {
        my $h = open_dir_at($ph, $name) or bail("$rel changed while copying", 6);
        rec("D", "$rel\0", ($l[2] & 07777), "\0$l[9]\0");
        for my $k (names_in($h)) {
            if ($k =~ /[\x00-\x1f\x7f]/) { rec("S"); next }
            send_at($h, $k, "$rel/$k");
        }
        closedir($h);
    } elsif (S_ISREG($l[2])) {
        chdir($ph);
        sysopen(my $f, $name, O_RDONLY | O_NOFOLLOW | O_NONBLOCK) or bail("$rel changed while copying ($!)", 6);
        my @s = stat($f);
        (@s && S_ISREG($s[2]) && $s[0] == $l[0] && $s[1] == $l[1]) or bail("$rel changed while copying", 6);
        my $left = $s[7];
        rec("F", "$rel\0", ($s[2] & 07777), "\0$s[9]\0$left\0");
        while ($left > 0) {
            my $n = sysread($f, my $buf, $left < 262144 ? $left : 262144);
            defined $n && $n > 0 or bail("$rel got shorter while copying", 6);
            print $buf; $left -= $n;
        }
        close($f);
    } else {
        rec("S");
    }
}
sub cmd_send {
    my $parent = field(); my $count = field();
    my @names = map { field() } 1 .. $count;
    my $P = open_top($parent);
    my $total = 0; $total += size_at($P, $_) for @names;
    rec("T", "$total\0");
    send_at($P, $_, $_) for @names;
    rec("Z");
}

# ---- recv: write a stream into a new private folder inside $dst, then
# move its items into place, then remove it. The folder is held open from
# the moment it is made until it is gone, and never looked up by name again.
sub cmd_recv {
    my $dst = field(); my $stage = field(); my $mode = field();
    my $D = open_top($dst);
    chdir($D) or bail("Can't enter $dst", 3);
    umask(077);
    mkdir($stage, 0700) or bail("Can't write to $dst ($!)", 3);
    my $S = open_dir_at($D, $stage) or bail("The new folder in $dst changed", 6);
    check_stage($S) or bail("The new folder in $dst was swapped for another one", 6);
    $UNDO = [$D, $stage, $S];
    # If this computer goes away while the reply is being written, still
    # remove the stage (through its handle) before exiting.
    $SIG{PIPE} = sub { $SIG{PIPE} = 'IGNORE'; bail("stopped", 141) };
    my @stack = (["", $S]);
    my $close = sub {   # a finished folder gets its mode and time
        my $x = pop(@stack);
        chdir($x->[2]) or return;
        chmod(($x->[3] & 0777) | 0700, $x->[4]); utime($x->[5], $x->[5], $x->[4]);
    };
    while (1) {
        my $t; read(STDIN, $t, 1) == 1 or bail("short input", 2);
        last if $t eq "Z";
        my $p = field();
        my ($par, $name) = $p =~ m{^(?:(.*)/)?([^/]+)$} or bail("bad name", 2);
        $par = "" unless defined $par;
        ($name ne '.' && $name ne '..') or bail("bad name", 2);
        $close->() while @stack > 1 && $stack[-1][0] ne $par;
        $stack[-1][0] eq $par or bail("out of order: $p", 2);
        my $ph = $stack[-1][1];
        chdir($ph) or bail("lost a folder", 6);
        if ($t eq "D") {
            my $m = field(); my $mt = field();
            @stack < 512 or bail("folders nested too deeply", 2);
            mkdir($name, 0700) or bail("Can't create $p ($!)", 1);
            my $h = open_dir_at($ph, $name) or bail("$p changed", 6);
            push @stack, [$p, $h, $ph, $m, $name, $mt];
        } elsif ($t eq "F") {
            my $m = field(); my $mt = field(); my $left = field();
            sysopen(my $f, $name, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW, 0600) or bail("Can't create $p ($!)", 1);
            while ($left > 0) {
                my $n = read(STDIN, my $buf, $left < 262144 ? $left : 262144);
                defined $n && $n > 0 or bail("short input", 2);
                defined syswrite($f, $buf) or bail("Can't write $p ($!)", 1);
                $left -= $n;
            }
            chmod($m & 0777, $f); utime($mt, $mt, $f);
            close($f) or bail("Can't write $p ($!)", 1);
        } elsif ($t eq "L") {
            my $target = field();
            symlink($target, $name) or bail("Can't create $p ($!)", 1);
        } else {
            bail("bad record", 2);
        }
    }
    $close->() while @stack > 1;
    place_in($S, $D, "", $mode eq "replace");
    drop_stage($D, $stage, $S);
    rec("Z");
}

# ---- place: move the stage's items into $dst. Without replace, an atomic
# rename that refuses to replace anything; with replace, files and links
# replace files and links atomically and folders merge into real folders.
sub place_in {
    my ($sh, $dh, $rel, $replace, $depth) = @_;
    for my $n (names_in($sh)) {
        my $r = $rel eq "" ? $n : "$rel/$n";
        chdir($sh) or bail("lost a folder", 6);
        my @m = lstat($n) or next;
        next if rename_at(fd_of($sh), $n, fd_of($dh), $n, 1);
        if (unsafe_fs()) { rec("E", "$r\0"); next }
        if ($! + 0 != EEXIST) { rec("E", "$r\0"); next }
        if (!$replace) { rec("C", "$r\0"); next }
        chdir($dh) or bail("lost a folder", 6);
        my @t = lstat($n) or do { rec("C", "$r\0"); next };
        if (S_ISDIR($m[2]) && S_ISDIR($t[2]) && ($depth || 0) < 512) {
            my $cs = open_dir_at($sh, $n) or do { rec("E", "$r\0"); next };
            my $cd = open_dir_at($dh, $n) or do { rec("C", "$r\0"); next };
            place_in($cs, $cd, $r, 1, ($depth || 0) + 1);
            closedir($cs); closedir($cd);
        } elsif (!S_ISDIR($m[2]) && !S_ISDIR($t[2])) {
            rename_at(fd_of($sh), $n, fd_of($dh), $n, 0) or rec("C", "$r\0");
        } else {
            rec("C", "$r\0");
        }
    }
}

# ---- rename one item inside a folder without replacing anything.
sub cmd_rename {
    my $dir = field(); my $old = field(); my $new = field();
    my $D = open_top($dir);
    exit 0 if rename_at(fd_of($D), $old, fd_of($D), $new, 1);
    bail("exists", 17) if $! + 0 == EEXIST;
    bail("unsafe", 22) if unsafe_fs();
    bail("$!", 1);
}

my $cmd = field();
if ($cmd eq "send") { cmd_send() } elsif ($cmd eq "recv") { cmd_recv() }
elsif ($cmd eq "rename") { cmd_rename() }
else { bail("unknown command", 2) }
exit 0;
