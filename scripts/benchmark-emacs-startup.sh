#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

bin=${ZS_BIN:-}
if [[ -z "$bin" ]]; then
  if command -v zerostack >/dev/null 2>&1; then
    bin=$(command -v zerostack)
  else
    bin="$root/target/debug/zerostack"
  fi
fi

id=11111111-1111-4111-8111-111111111111
turns=${ZS_BENCH_TURNS:-80}
mkdir -p "$work/data/sessions" "$work/config" "$work/runtime"
printf '' >"$work/data/shown_welcome_msg"

perl -MJSON::PP -MTime::Piece - "$work/data/sessions/$id.json" "$root" "$turns" <<'PL'
use strict;
use warnings;
my ($path, $root, $turns) = @ARGV;
my $now = gmtime->datetime . "Z";
my $code = join("\n", map { qq{fn generated_$_() { println!("line $_"); }} } 0..159);
my $para = join(" ", ("markdown-heavy session replay benchmark") x 220);
my @messages;
for my $i (0..($turns - 1)) {
    push @messages, { role => "user", content => "Generate and explain block $i.", estimated_tokens => 8 };
    my $table = join("\n", map { "| $_ | **bold** | `code` |" } 0..39);
    push @messages, { role => "assistant", content => "# Block $i\n\n$para\n\n```rust\n$code\n```\n\n| a | b | c |\n|---|---|---|\n$table", estimated_tokens => 900 };
    push @messages, { role => "tool_call", content => qq{bash {"command":"printf block-$i"}}, estimated_tokens => 12 };
    my $tool = join("\n", map { "\e[31mtool result $i.$_\e[0m lorem ipsum" } 0..179);
    push @messages, { role => "tool_result", content => "bash output:\n$tool", estimated_tokens => 700 };
}
my $total = 0;
$total += $_->{estimated_tokens} for @messages;
my $obj = {
    id => "11111111-1111-4111-8111-111111111111", name => "heavy startup benchmark", messages => \@messages, compactions => [],
    created_at => $now, updated_at => $now, total_input_tokens => 0, total_output_tokens => 0, total_cost => 0,
    total_estimated_tokens => $total, calibrated_tokens => 0, calibrated_msg_count => 0,
    input_token_cost => 0, output_token_cost => 0, context_window => 200000, model => "dummy", provider => "ollama",
    working_dir => $root, permission_allowlist => [],
};
open my $fh, ">", $path or die $!;
print {$fh} JSON::PP->new->canonical->encode($obj);
print "$path\n";
PL

run_args=(--emacs --provider ollama --model dummy --session "$id" --no-context-files --no-color)
profile="$work/profile-emacs.tsv"
socket="$work/runtime/zerostack/sessions/$id/sock"

XDG_RUNTIME_DIR="$work/runtime" ZS_DATA_DIR="$work/data" ZS_CONFIG_DIR="$work/config" ZS_STARTUP_PROFILE="$profile" TERM=xterm-256color \
  "$bin" "${run_args[@]}" >"$work/server.out" 2>"$work/server.err" &
server_pid=$!
trap 'kill "$server_pid" 2>/dev/null || true; rm -rf "$work"' EXIT

for _ in $(seq 1 200); do
  [[ -S "$socket" ]] && break
  sleep 0.025
done
[[ -S "$socket" ]] || { cat "$work/server.err" >&2; echo "socket not ready: $socket" >&2; exit 1; }

elisp="$work/bench.el"
cat >"$elisp" <<EOF
(require 'cl-lib)
(load-file "$root/emacs/zerostack.el")
(setq zerostack-command "$bin")
(defvar zs-bench-start nil)
(defvar zs-bench-first-render nil)
(defvar zs-bench-done nil)
(defvar zs-bench-bytes 0)
(defvar zs-bench-lines 0)
(defvar zs-bench-last-change nil)
(defun zs-bench-count-chunk (orig chunk)
  (setq zs-bench-bytes (+ zs-bench-bytes (string-bytes chunk)))
  (setq zs-bench-lines (+ zs-bench-lines (cl-count ?\n chunk)))
  (setq zs-bench-last-change (float-time))
  (funcall orig chunk))
(defun zs-bench-render-advice (orig replace-from lines)
  (unless zs-bench-first-render (setq zs-bench-first-render (float-time)))
  (setq zs-bench-last-change (float-time))
  (funcall orig replace-from lines))
(defun zs-bench-prepend-advice (orig lines)
  (prog1 (funcall orig lines)
    (setq zs-bench-last-change (float-time))))
(advice-add 'zerostack--consume-chunk :around #'zs-bench-count-chunk)
(advice-add 'zerostack--replace-lines :around #'zs-bench-render-advice)
(advice-add 'zerostack--prepend-lines :around #'zs-bench-prepend-advice)
(let* ((socket "$socket")
       (default-directory "$root/")
       (zs-bench-start (float-time))
       (deadline (+ (float-time) 120.0))
       (buffer (zerostack-connect socket "bench" "$root" "$root" "$id")))
  (while (and (< (float-time) deadline)
              (not (and zs-bench-first-render
                        (or (null zs-bench-last-change)
                            (> (- (float-time) zs-bench-last-change) 0.5))
                        (with-current-buffer buffer
                          (and (null zerostack--backfill-queue)
                               (null zerostack--backfill-timer))))))
    (accept-process-output nil 0.05))
  (when (buffer-live-p buffer)
    (with-current-buffer buffer
      (when (process-live-p zerostack--process)
        (delete-process zerostack--process))))
  (princ (format "emacs_connect_to_first_render_ms\t%.3f\nemacs_connect_to_idle_ms\t%.3f\nemacs_bytes\t%d\nemacs_protocol_lines\t%d\nemacs_buffer_chars\t%d\nemacs_rendered_lines\t%d\n"
                 (* 1000.0 (- (or zs-bench-first-render (float-time)) zs-bench-start))
                 (* 1000.0 (- (float-time) zs-bench-start))
                 zs-bench-bytes zs-bench-lines
                 (if (buffer-live-p buffer) (with-current-buffer buffer (buffer-size)) -1)
                 (if (buffer-live-p buffer) (with-current-buffer buffer (length zerostack--line-markers)) -1))))
EOF
emacs --quick --batch --load "$elisp"

kill "$server_pid" 2>/dev/null || true
wait "$server_pid" 2>/dev/null || true
cat "$profile"
