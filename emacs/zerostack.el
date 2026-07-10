;;; zerostack.el --- Native zerostack client -*- lexical-binding: t; -*-

;; Copyright (C) 2026
;; SPDX-License-Identifier: GPL-3.0-only

;;; Commentary:
;; ERC-style Emacs client for `zerostack --emacs'.  The Rust side sends
;; pre-rendered markdown as line plists; this client only applies faces,
;; clickable artifact links, and optional inline AUCTeX folding.

;;; Code:

(require 'cl-lib)
(require 'button)
(require 'subr-x)
(require 'url-util nil t)
(require 'project nil t)
(require 'hydra nil t)
(require 'yank-media nil t)

(defgroup zerostack nil
  "Native Emacs client for zerostack."
  :group 'tools)

(defcustom zerostack-command "zerostack"
  "Command used to start a native zerostack Emacs session."
  :type 'string)

(defcustom zerostack-default-cols 100
  "Initial markdown render width sent to zerostack."
  :type 'integer)

(defcustom zerostack-buffer-name "*zerostack*"
  "Default zerostack chat buffer name."
  :type 'string)

(defcustom zerostack-board-buffer-name "*zerostack board*"
  "Default zerostack project board buffer name."
  :type 'string)

(defcustom zerostack-board-directories nil
  "Extra directories to show on `zerostack-board'.

These are Emacs-side pins for directories that may not have any saved
zerostack sessions yet.  They are hidden automatically when the Rust board
snapshot already contains the directory as a project, worktree, workspace, or
session cwd."
  :type '(repeat directory))

(defcustom zerostack-auctex-preview t
  "When non-nil, render LaTeX spans in place with AUCTeX helpers.

Automatic LaTeX rendering is strictly inline: source artifacts are not displayed
unless explicitly opened with `/latex' or `zerostack-open-artifact-at-point'."
  :type 'boolean)

(defcustom zerostack-auctex-display-buffer nil
  "Obsolete compatibility option; automatic LaTeX rendering stays inline."
  :type 'boolean)

(defcustom zerostack-auctex-fold t
  "When non-nil, use AUCTeX `TeX-fold-mode' for inline LaTeX display.

This gives chat-buffer LaTeX spans lightweight Unicode-style folding for common
math macros while keeping the original LaTeX source and artifact link intact."
  :type 'boolean)

(defcustom zerostack-prompt "zs> "
  "Prompt displayed at the bottom of zerostack chat buffers."
  :type 'string)

(defface zerostack-normal-face
  '((t :inherit default))
  "Normal zerostack output face.")

(defface zerostack-heading-face
  '((t :inherit font-lock-function-name-face :weight bold))
  "Heading zerostack output face.")

(defface zerostack-code-face
  '((t :inherit fixed-pitch :background unspecified))
  "Code zerostack output face.")

(defface zerostack-code-block-face
  '((t :inherit fixed-pitch :background unspecified))
  "Code block zerostack output face.")

(defface zerostack-table-face
  '((t :inherit fixed-pitch))
  "Table zerostack output face.")

(defface zerostack-table-border-face
  '((t :inherit (fixed-pitch shadow)))
  "Table border zerostack output face.")

(defface zerostack-bold-face
  '((t :weight bold))
  "Bold zerostack output face.")

(defface zerostack-italic-face
  '((t :slant italic))
  "Italic zerostack output face.")

(defface zerostack-quote-face
  '((t :inherit font-lock-comment-face))
  "Block quote zerostack output face.")

(defface zerostack-list-marker-face
  '((t :inherit font-lock-keyword-face :weight bold))
  "List marker zerostack output face.")

(defface zerostack-muted-face
  '((t :inherit shadow))
  "Muted zerostack output face.")

(defface zerostack-link-face
  '((t :inherit link))
  "Clickable zerostack artifact face.")

(defface zerostack-user-face
  '((t :inherit font-lock-string-face))
  "User message face.")

(defface zerostack-tool-face
  '((t :inherit font-lock-keyword-face))
  "Tool event face.")

(defface zerostack-reasoning-face
  '((t :inherit font-lock-comment-face :slant italic))
  "Reasoning artifact face.")

(defface zerostack-error-face
  '((t :inherit error))
  "Error face.")

(defface zerostack-prompt-face
  '((t :inherit minibuffer-prompt :weight bold))
  "Input prompt face.")

(defface zerostack-latex-face
  '((t :inherit default :underline nil))
  "Face used for LaTeX spans with preview metadata.")

(defface zerostack-board-project-face
  '((t :inherit font-lock-function-name-face :weight bold))
  "Face used for zerostack board project rows.")

(defface zerostack-board-worktree-face
  '((t :inherit font-lock-variable-name-face))
  "Face used for zerostack board worktree rows.")

(defface zerostack-board-session-face
  '((t :inherit default))
  "Face used for zerostack board session rows.")

(defface zerostack-board-alive-face
  '((t :inherit success :weight bold))
  "Face used for zerostack board rows with live sessions.")

(defface zerostack-board-thinking-face
  '((t :foreground "yellow" :weight bold))
  "Face used for zerostack board sessions that are currently thinking.")

(defface zerostack-board-input-face
  '((t :inherit warning :weight bold))
  "Face used for zerostack board sessions waiting for user input.")

(defconst zerostack--face-map
  '((zs-normal . zerostack-normal-face)
    (zs-heading . zerostack-heading-face)
    (zs-code . zerostack-code-face)
    (zs-code-block . zerostack-code-block-face)
    (zs-table . zerostack-table-face)
    (zs-table-border . zerostack-table-border-face)
    (zs-bold . zerostack-bold-face)
    (zs-italic . zerostack-italic-face)
    (zs-quote . zerostack-quote-face)
    (zs-list-marker . zerostack-list-marker-face)
    (zs-muted . zerostack-muted-face)
    (zs-link . zerostack-link-face)
    (zs-user . zerostack-user-face)
    (zs-tool . zerostack-tool-face)
    (zs-reasoning . zerostack-reasoning-face)
    (zs-error . zerostack-error-face))
  "Mapping from protocol face atoms to Emacs faces.")

(defvar zerostack-mode-map
  (let ((map (make-sparse-keymap)))
    (define-key map (kbd "RET") #'zerostack-send-input)
    (define-key map (kbd "C-y") #'zerostack-yank)
    (define-key map [remap yank] #'zerostack-yank)
    (define-key map (kbd "C-RET") #'zerostack-insert-newline)
    (define-key map (kbd "C-<return>") #'zerostack-insert-newline)
    (define-key map (kbd "M-RET") #'zerostack-insert-newline)
    (define-key map (kbd "M-<return>") #'zerostack-insert-newline)
    (define-key map (kbd "C-c C-c") #'zerostack-abort)
    (define-key map (kbd "C-c C-m") #'zerostack-command-menu)
    (define-key map (kbd "C-c /") #'zerostack-command-menu)
    (define-key map (kbd "C-c C-a") #'zerostack-attach)
    (define-key map (kbd "C-c C-o") #'zerostack-open-artifact-at-point)
    (define-key map (kbd "C-c C-s") #'zerostack-request-status)
    (define-key map (kbd "C-c C-w") #'zerostack-rewind)
    (define-key map (kbd "C-c C-r") #'zerostack-redo)
    map)
  "Keymap for `zerostack-mode'.")

(defvar zerostack-artifact-map
  (let ((map (make-sparse-keymap)))
    (define-key map (kbd "RET") #'zerostack-open-artifact-at-point)
    (define-key map [mouse-2] #'zerostack-open-artifact-at-point)
    map)
  "Keymap used on clickable artifact and LaTeX spans.")

(defun zerostack-board--bind-keys (map)
  (set-keymap-parent map special-mode-map)
  (define-key map (kbd "g") #'zerostack-board-refresh)
  (define-key map (kbd "j") #'zerostack-board-jump)
  (define-key map (kbd "o") #'zerostack-board-open)
  (define-key map (kbd "A") #'zerostack-board-open-attention)
  (define-key map (kbd "c") #'zerostack-board-create-at-point)
  (define-key map (kbd "d") #'zerostack-board-set-description-at-point)
  (define-key map (kbd "s") #'zerostack-board-stop-at-point)
  (define-key map (kbd "x") #'zerostack-board-trash-at-point)
  (define-key map (kbd "RET") #'zerostack-board-open-at-point)
  (define-key map [mouse-2] #'zerostack-board-open-at-point)
  map)

(defvar zerostack-board-mode-map
  (zerostack-board--bind-keys (make-sparse-keymap))
  "Keymap for `zerostack-board-mode'.")

(zerostack-board--bind-keys zerostack-board-mode-map)

(defvar zerostack-global-mode-map
  (let ((map (make-sparse-keymap)))
    (define-key map (kbd "C-c z") #'zerostack-board-add-current-directory)
    map)
  "Global keymap for zerostack convenience commands.")

(defvar-local zerostack--process nil)
(defvar-local zerostack--server-process nil)
(defvar-local zerostack--server-args nil)
(defvar-local zerostack--startup-timer nil)
(defvar-local zerostack--socket nil)
(defvar-local zerostack--session nil)
(defvar-local zerostack--pid nil)
(defvar-local zerostack--session-title nil)
(defvar-local zerostack--cwd nil)
(defvar-local zerostack--worktree-dir nil)
(defvar-local zerostack--provider nil)
(defvar-local zerostack--model nil)
(defvar-local zerostack--subagent-provider nil)
(defvar-local zerostack--subagent-model nil)
(defvar-local zerostack--tokens nil)
(defvar-local zerostack--reasoning-tokens nil)
(defvar-local zerostack--context-window nil)
(defvar-local zerostack--protocol nil)
(defvar-local zerostack--cols nil)
(defvar-local zerostack--metadata-status-request nil)
(defvar-local zerostack--line-markers nil)
(defvar-local zerostack--backfill-queue nil)
(defvar-local zerostack--backfill-timer nil)
(defvar-local zerostack--notice-start-marker nil)
(defvar-local zerostack--prompt-start-marker nil)
(defvar-local zerostack--input-marker nil)
(defvar-local zerostack--controls-start-marker nil)
(defvar-local zerostack--line-buffer "")
(defvar-local zerostack--request-counter 0)
(defvar-local zerostack--thinking nil)
(defvar-local zerostack--loop-active nil)
(defvar-local zerostack--loop-label nil)
(defvar-local zerostack--thinking-level "on")
(defvar-local zerostack--reasoning-effort-supported nil)
(defvar-local zerostack--reasoning-effort nil)
(defvar-local zerostack--reasoning-efforts nil)
(defvar-local zerostack--status nil)
(defvar-local zerostack--notice nil)
(defvar-local zerostack--notice-timer nil)
(defvar-local zerostack--ready-notify-timer nil)
(defvar-local zerostack--send-function nil)
(defvar-local zerostack--pending-permissions nil)
(defvar-local zerostack--artifacts nil)
(defvar-local zerostack--clipboard-temp-files nil)
(defvar-local zerostack--latex-items nil)
(defvar-local zerostack--latex-overlays nil)
(defvar-local zerostack--last-notice nil)

(defcustom zerostack-notify-on-ready t
  "When non-nil, send notify-send when a session becomes ready for input."
  :type 'boolean)

(defcustom zerostack-notice-timeout 2.0
  "Seconds before transient zerostack prompt notices are cleared."
  :type 'number)

(defvar-local zerostack-board--snapshot nil)
(defvar-local zerostack-board--fetch-function nil)
(defvar-local zerostack-board--session-limits nil)
(defvar zerostack--config-command-function nil
  "Optional test hook used instead of invoking `zerostack-command config'.")

(defmacro zerostack--without-undo (&rest body)
  "Run BODY without recording buffer undo entries."
  (declare (indent 0) (debug t))
  `(let ((buffer-undo-list t))
     ,@body))

;;;###autoload
(define-derived-mode zerostack-mode fundamental-mode "zerostack"
  "Major mode for native zerostack sessions."
  (setq-local zerostack--cols zerostack-default-cols)
  (setq-local zerostack--session-title nil)
  (setq-local zerostack--cwd default-directory)
  (setq-local zerostack--worktree-dir default-directory)
  (setq-local zerostack--provider nil)
  (setq-local zerostack--model nil)
  (setq-local zerostack--subagent-provider nil)
  (setq-local zerostack--subagent-model nil)
  (setq-local zerostack--tokens nil)
  (setq-local zerostack--reasoning-tokens nil)
  (setq-local zerostack--context-window nil)
  (setq-local zerostack--metadata-status-request nil)
  (setq-local zerostack--line-markers nil)
  (setq-local zerostack--backfill-queue nil)
  (setq-local zerostack--backfill-timer nil)
  (setq-local zerostack--notice-start-marker nil)
  (setq-local zerostack--controls-start-marker nil)
  (setq-local zerostack--line-buffer "")
  (setq-local zerostack--request-counter 0)
  (setq-local zerostack--thinking nil)
  (setq-local zerostack--loop-active nil)
  (setq-local zerostack--loop-label nil)
  (setq-local zerostack--thinking-level "on")
  (setq-local zerostack--reasoning-effort-supported nil)
  (setq-local zerostack--reasoning-effort nil)
  (setq-local zerostack--reasoning-efforts nil)
  (setq-local zerostack--status nil)
   (setq-local zerostack--notice nil)
   (setq-local zerostack--notice-timer nil)
   (setq-local zerostack--ready-notify-timer nil)
   (setq-local zerostack--pending-permissions (make-hash-table :test 'eql))
  (setq-local zerostack--latex-items (make-hash-table :test 'equal))
  (setq-local zerostack--artifacts nil)
  (setq-local zerostack--clipboard-temp-files nil)
  (setq-local zerostack--latex-overlays nil)
  (setq-local zerostack--last-notice nil)
  (setq truncate-lines nil)
  (when (fboundp 'yank-media-handler)
    (yank-media-handler "\\`image/" #'zerostack--yank-media-image))
  (zerostack--ensure-prompt))

(defun zerostack--chat-buffer-name (&optional title dir)
  "Return a chat buffer name for session TITLE in worktree DIR."
  (format "*zerostack: %s @ %s*"
          (zerostack--buffer-component (or title "session"))
          (zerostack--buffer-component
           (if (and dir (not (string-empty-p dir)))
               (file-name-nondirectory (directory-file-name dir))
             "worktree"))))

(defun zerostack--buffer-component (value)
  "Return VALUE normalized for use inside a buffer name."
  (let ((text (zerostack--status-text (format "%s" (or value "")))))
    (if (string-empty-p text) "-" text)))

(defun zerostack--set-session-metadata (&optional title cwd worktree-dir)
  "Store session TITLE, CWD, and WORKTREE-DIR and rename the buffer."
  (when (and title (not (string-empty-p title)))
    (setq zerostack--session-title title))
  (when (and cwd (not (string-empty-p cwd)))
    (setq zerostack--cwd (file-name-as-directory cwd)))
  (when (and worktree-dir (not (string-empty-p worktree-dir)))
    (setq zerostack--worktree-dir (file-name-as-directory worktree-dir)))
  (when-let ((dir (or zerostack--worktree-dir zerostack--cwd)))
    (when (file-directory-p dir)
      (setq default-directory (file-name-as-directory dir))))
  (zerostack--rename-chat-buffer))

(defun zerostack--rename-chat-buffer ()
  "Rename the current chat buffer from known session metadata."
  (let ((title (or zerostack--session-title
                   (and zerostack--session
                        (if (> (length zerostack--session) 8)
                            (substring zerostack--session 0 8)
                          zerostack--session))
                   "session"))
        (dir (or zerostack--worktree-dir zerostack--cwd default-directory)))
    (rename-buffer (zerostack--chat-buffer-name title dir) t)))

(defun zerostack--normalize-socket (socket)
  "Return SOCKET normalized for buffer matching."
  (when (and socket (not (string-empty-p socket)))
    (expand-file-name socket)))

(defun zerostack--session-id-from-args (args)
  "Return a `--session' value from ARGS, when present."
  (cl-loop for rest on args
           for arg = (car rest)
           when (and (stringp arg) (string= arg "--session"))
           return (cadr rest)
           when (and (stringp arg) (string-prefix-p "--session=" arg))
           return (substring arg (length "--session="))))

(defun zerostack--chat-buffer-p (buffer)
  "Return non-nil when BUFFER is a zerostack chat buffer."
  (and (buffer-live-p buffer)
       (with-current-buffer buffer
         (derived-mode-p 'zerostack-mode))))

(defun zerostack--find-chat-buffer (&optional session socket exclude)
  "Find a chat buffer for SESSION or SOCKET, excluding EXCLUDE."
  (let ((socket (zerostack--normalize-socket socket)))
    (cl-find-if
     (lambda (buffer)
       (and (not (eq buffer exclude))
            (zerostack--chat-buffer-p buffer)
            (with-current-buffer buffer
              (or (and session zerostack--session
                       (equal session zerostack--session))
                  (and socket zerostack--socket
                       (equal socket (zerostack--normalize-socket zerostack--socket)))))))
     (buffer-list))))

(defun zerostack--connected-to-socket-p (socket)
  "Return non-nil if current buffer has a live process connected to SOCKET."
  (and (process-live-p zerostack--process)
       (equal (zerostack--normalize-socket socket)
              (zerostack--normalize-socket zerostack--socket))))

(defun zerostack--startup-active-p ()
  "Return non-nil when current buffer is still starting a worker."
  (or zerostack--startup-timer
      (process-live-p zerostack--server-process)))

(defun zerostack--delete-current-processes ()
  "Delete processes and timers owned by the current zerostack buffer."
  (when zerostack--startup-timer
    (cancel-timer zerostack--startup-timer)
    (setq zerostack--startup-timer nil))
  (when zerostack--notice-timer
    (cancel-timer zerostack--notice-timer)
    (setq zerostack--notice-timer nil))
  (when (process-live-p zerostack--process)
    (delete-process zerostack--process))
  (setq zerostack--process nil)
  (when (process-live-p zerostack--server-process)
    (delete-process zerostack--server-process))
  (setq zerostack--server-process nil))

(defun zerostack--dedupe-current-chat-buffer ()
  "Ensure current session/socket is represented by only one live chat buffer.

When another live buffer already owns this session, schedule this duplicate buffer
for removal and show the existing buffer.  When the duplicate is stale, remove the
stale buffer and keep the current connection."
  (when-let ((duplicate (zerostack--find-chat-buffer zerostack--session
                                                     zerostack--socket
                                                     (current-buffer))))
    (if (with-current-buffer duplicate (process-live-p zerostack--process))
        (let ((current (current-buffer)))
          (zerostack--delete-current-processes)
          (run-at-time
           0 nil
           (lambda (buffer existing)
             (when (buffer-live-p buffer)
               (kill-buffer buffer))
             (when (buffer-live-p existing)
               (pop-to-buffer existing)))
           current duplicate)
          t)
      (kill-buffer duplicate)
      nil)))

;;;###autoload
(defun zerostack (&optional args title cwd worktree-dir session-id)
  "Start `zerostack --emacs' and connect to its native socket.

With prefix argument, read extra command-line ARGS."
  (interactive
   (list (when current-prefix-arg
           (split-string-shell-command (read-shell-command "zerostack args: ")))))
  (let* ((args (or args nil))
         (session-id (or session-id (zerostack--session-id-from-args args)))
         (existing (and session-id (zerostack--find-chat-buffer session-id nil))))
    (if existing
        (progn
          (with-current-buffer existing
            (setq zerostack--session session-id)
            (zerostack--set-session-metadata title cwd worktree-dir)
            (unless (or (process-live-p zerostack--process)
                        (zerostack--startup-active-p))
              (zerostack--append-local-line "starting zerostack --emacs" 'zs-muted)
              (zerostack--start-server args)))
          (pop-to-buffer existing)
          existing)
      (let ((buffer (generate-new-buffer
                     (zerostack--chat-buffer-name title (or worktree-dir cwd default-directory)))))
        (with-current-buffer buffer
          (zerostack-mode)
          (setq zerostack--session session-id)
          (zerostack--set-session-metadata title cwd worktree-dir)
          (zerostack--append-local-line "starting zerostack --emacs" 'zs-muted)
          (zerostack--start-server args))
        (pop-to-buffer buffer)
        buffer))))

;;;###autoload
(defun zerostack-connect (socket &optional title cwd worktree-dir session-id)
  "Connect to an existing native zerostack SOCKET."
  (interactive "fzerostack socket: ")
  (let* ((socket (zerostack--normalize-socket socket))
         (existing (zerostack--find-chat-buffer session-id socket)))
    (if existing
        (progn
          (with-current-buffer existing
            (when session-id
              (setq zerostack--session session-id))
            (zerostack--set-session-metadata title cwd worktree-dir)
            (unless (zerostack--connected-to-socket-p socket)
              (zerostack--delete-current-processes)
              (zerostack--connect-buffer socket)))
          (pop-to-buffer existing)
          existing)
      (let ((buffer (generate-new-buffer
                     (zerostack--chat-buffer-name title (or worktree-dir cwd default-directory)))))
        (with-current-buffer buffer
          (zerostack-mode)
          (setq zerostack--session session-id)
          (zerostack--set-session-metadata title cwd worktree-dir)
          (zerostack--connect-buffer socket))
        (pop-to-buffer buffer)
        buffer))))

;;;###autoload
(defun zerostack-list-sessions ()
  "Show live native Emacs zerostack sessions using `zerostack --emacs-list'."
  (interactive)
  (let ((buffer (get-buffer-create "*zerostack sessions*")))
    (with-current-buffer buffer
      (let ((inhibit-read-only t))
        (erase-buffer)
        (let ((status (call-process zerostack-command nil buffer nil "--emacs-list")))
          (unless (zerop status)
            (goto-char (point-max))
            (insert (format "\nzerostack --emacs-list exited with %s\n" status))))
        (special-mode)))
    (pop-to-buffer buffer)))

;;;###autoload
(defun zerostack-board ()
  "Show the zerostack project/worktree/session board."
  (interactive)
  (let ((buffer (get-buffer-create zerostack-board-buffer-name)))
    (with-current-buffer buffer
      (unless (derived-mode-p 'zerostack-board-mode)
        (zerostack-board-mode))
      (zerostack-board-refresh))
    (pop-to-buffer buffer)))

;;;###autoload
(defun zerostack-board-add-current-directory ()
  "Add the current project or directory to `zerostack-board' and show it.

The root is resolved with Projectile when available, then `project.el', then
`default-directory'.  Existing board entries are not duplicated."
  (interactive)
  (let* ((dir (zerostack-board--current-directory-root))
         (snapshot (ignore-errors (zerostack-board--fetch)))
         (already (and snapshot (zerostack-board--snapshot-has-directory-p snapshot dir)))
         (added (and (not already) (zerostack-board--remember-directory dir)))
         (buffer (get-buffer-create zerostack-board-buffer-name)))
    (cond
     (already (message "zerostack board already contains %s" dir))
     (added (message "Added %s to zerostack board" dir))
     (t (message "zerostack board already has %s pinned" dir)))
    (with-current-buffer buffer
      (unless (derived-mode-p 'zerostack-board-mode)
        (zerostack-board-mode))
      (if snapshot
          (progn
            (setq zerostack-board--snapshot snapshot)
            (zerostack-board--render snapshot))
        (zerostack-board-refresh)))
    (pop-to-buffer buffer)))

;;;###autoload
(define-minor-mode zerostack-global-mode
  "Global zerostack keybindings."
  :global t
  :keymap zerostack-global-mode-map)

(zerostack-global-mode 1)

;;;###autoload
(define-derived-mode zerostack-board-mode special-mode "zerostack-board"
  "Major mode for the zerostack project/worktree/session board."
  (setq truncate-lines t)
  (setq-local zerostack-board--session-limits (make-hash-table :test 'equal))
  (setq-local revert-buffer-function #'zerostack-board--revert))

(defun zerostack-board--revert (_ignore-auto _noconfirm)
  "Refresh a zerostack board buffer for `revert-buffer'."
  (zerostack-board-refresh))

(defun zerostack-board-refresh ()
  "Refresh the zerostack board snapshot."
  (interactive)
  (let ((line (line-number-at-pos))
        (column (current-column))
        (snapshot (zerostack-board--fetch)))
    (setq zerostack-board--snapshot snapshot)
    (zerostack-board--render snapshot)
    (zerostack--goto-line-column line column)))

(defun zerostack-board--refresh-if-visible ()
  "Refresh the board buffer when it already exists."
  (when-let ((buffer (get-buffer zerostack-board-buffer-name)))
    (run-at-time
     0 nil
     (lambda ()
       (when (buffer-live-p buffer)
         (with-current-buffer buffer
           (zerostack-board-refresh)))))))

(defun zerostack-board--fetch ()
  "Fetch and read one board snapshot S-expression."
  (if zerostack-board--fetch-function
      (funcall zerostack-board--fetch-function)
    (with-temp-buffer
      (let ((status (call-process zerostack-command nil t nil "--emacs-board")))
        (unless (zerop status)
          (error "zerostack --emacs-board exited with %s: %s"
                 status
                 (string-trim (buffer-string)))))
      (goto-char (point-min))
      (read (current-buffer)))))

(defun zerostack--config-command (&rest args)
  "Run `zerostack config' with ARGS and return trimmed stdout."
  (if zerostack--config-command-function
      (string-trim (apply zerostack--config-command-function args))
    (with-temp-buffer
      (let ((status (apply #'call-process zerostack-command nil t nil "config" args)))
        (unless (zerop status)
          (user-error "zerostack config %s exited with %s: %s"
                      (string-join args " ")
                      status
                      (string-trim (buffer-string)))))
      (string-trim (buffer-string)))))

(defun zerostack--config-lines (&rest args)
  "Run `zerostack config' with ARGS and return non-empty output lines."
  (split-string (apply #'zerostack--config-command args) "\n" t "[[:space:]]+"))

(defun zerostack--read-provider (&optional prompt default)
  "Read a provider name using zerostack config completion."
  (let ((providers (zerostack--config-lines "providers")))
    (unless providers
      (user-error "No zerostack providers available"))
    (completing-read (or prompt "Provider: ") providers nil t nil nil default)))

(defun zerostack--read-model (&optional provider default)
  "Read a model id for PROVIDER, allowing manual entry."
  (let ((models (if (and provider (not (string-empty-p provider)))
                    (zerostack--config-lines "models" provider)
                  (zerostack--config-lines "models"))))
    (completing-read (if (and provider (not (string-empty-p provider)))
                         (format "Model for %s: " provider)
                       "Model: ")
                     models nil nil nil nil default)))

(defun zerostack-board-set-default-provider ()
  "Switch the persisted default zerostack provider."
  (interactive)
  (let* ((provider (zerostack--read-provider "Default provider: "))
         (output (zerostack--config-command "set-provider" provider)))
    (zerostack-board-refresh)
    (message "zerostack default %s" (replace-regexp-in-string "\n" ", " output))))

(defun zerostack-board-set-default-model ()
  "Switch the persisted default zerostack model."
  (interactive)
  (let* ((model (zerostack--read-model nil nil))
         (output (zerostack--config-command "set-model" model)))
    (zerostack-board-refresh)
    (message "zerostack default %s" (replace-regexp-in-string "\n" ", " output))))

(defun zerostack-board-set-default-subagent-provider ()
  "Switch the persisted default zerostack subagent provider."
  (interactive)
  (let* ((provider (zerostack--read-provider "Default subagent provider: "))
         (output (zerostack--config-command "set-subagent-provider" provider)))
    (zerostack-board-refresh)
    (message "zerostack subagent default %s" (replace-regexp-in-string "\n" ", " output))))

(defun zerostack-board-set-default-subagent-model ()
  "Switch the persisted default zerostack subagent model."
  (interactive)
  (let* ((fields (cdr zerostack-board--snapshot))
         (provider (or (plist-get fields :subagent-provider)
                       (plist-get fields :provider)))
         (model (zerostack--read-model provider nil))
         (output (zerostack--config-command "set-subagent-model" model)))
    (zerostack-board-refresh)
    (message "zerostack subagent default %s" (replace-regexp-in-string "\n" ", " output))))

(defun zerostack-board--config-label (value fallback)
  "Return display label for a board config VALUE."
  (let ((text (and value (format "%s" value))))
    (if (and text (not (string-empty-p text))) text fallback)))

(defun zerostack-board--insert-config-button (label action)
  "Insert a board config button LABEL that calls ACTION."
  (insert-text-button label
                      'action (lambda (_) (call-interactively action))
                      'follow-link t
                      'face 'zerostack-link-face
                      'help-echo "RET selects this zerostack default"))

(defun zerostack-board--insert-config-controls (snapshot)
  "Insert clickable provider/model controls for main and subagent defaults."
  (let ((fields (cdr snapshot)))
    (insert "Main: ")
    (zerostack-board--insert-config-button
     (zerostack-board--config-label (plist-get fields :provider) "provider")
     #'zerostack-board-set-default-provider)
    (insert " / ")
    (zerostack-board--insert-config-button
     (zerostack-board--config-label (plist-get fields :model) "model")
     #'zerostack-board-set-default-model)
    (insert "\nSubagents: ")
    (zerostack-board--insert-config-button
     (zerostack-board--config-label (plist-get fields :subagent-provider) "provider")
     #'zerostack-board-set-default-subagent-provider)
    (insert " / ")
    (zerostack-board--insert-config-button
     (zerostack-board--config-label (plist-get fields :subagent-model) "model")
     #'zerostack-board-set-default-subagent-model))
  (insert "\n"))

(defun zerostack-board--render (snapshot)
  "Render board SNAPSHOT as a tree."
  (unless (eq (car-safe snapshot) 'zerostack-board)
    (error "not a zerostack board snapshot: %S" snapshot))
  (let* ((needs-attention (plist-get (cdr snapshot) :needs-attention))
         (projects (plist-get (cdr snapshot) :projects))
         (loose-workspaces (plist-get (cdr snapshot) :loose-workspaces))
         (pinned-workspaces (zerostack-board--pinned-workspaces snapshot))
         (all-workspaces (append loose-workspaces pinned-workspaces))
         (inhibit-read-only t))
    (erase-buffer)
    (insert (propertize "zerostack board\n" 'face 'zerostack-heading-face))
    (insert (propertize "g refresh, j jump, o open, A attention, RET open, c create, d describe, s stop, x trash\n" 'face 'zerostack-muted-face))
    (zerostack-board--insert-config-controls snapshot)
    (insert "\n")
    (when needs-attention
      (insert (propertize "needs attention\n" 'face 'zerostack-heading-face))
      (dolist (session needs-attention)
        (zerostack-board--insert-attention-session session))
      (insert "\n"))
    (if (or needs-attention projects all-workspaces)
        (progn
          (dolist (project projects)
            (zerostack-board--insert-project project))
          (when all-workspaces
            (insert "\n")
            (insert (propertize "other workspaces\n" 'face 'zerostack-heading-face))
            (dolist (workspace all-workspaces)
              (zerostack-board--insert-loose-workspace workspace))))
      (insert (propertize "no saved sessions\n" 'face 'zerostack-muted-face)))))

(defun zerostack-board--insert-project (project)
  "Insert one PROJECT node and its children."
  (let* ((worktrees (plist-get project :worktrees))
         (active-worktrees (zerostack-board--active-worktrees worktrees))
         (single-active (and (= (length active-worktrees) 1) (car active-worktrees)))
         (key (format "project:%s" (or (plist-get project :path) "")))
         (limit (zerostack-board--session-limit key))
         (collapsed (and single-active
                         (not (zerostack-board--session-limit-set-p key))))
         (shown-worktrees (if collapsed nil (cl-subseq worktrees 0 (min limit (length worktrees)))))
         (face (and single-active
                    (zerostack-board--worktree-face single-active)))
         (item (append (list :type 'project
                             :path (plist-get project :path)
                             :repo (plist-get project :repo)
                             :name (plist-get project :name))
                       (when single-active
                         (list :workspace-item
                               (zerostack-board--worktree-item-with-session project single-active))))))
    (if collapsed
        (zerostack-board--insert-project-row
         (format "%s project %s  %s"
                 (zerostack-board--alive-marker (plist-get project :alive))
                 (or (plist-get project :name) "")
                 (or (plist-get project :path) ""))
         face
         item
         (list key (length worktrees) 0))
      (zerostack-board--insert-row
       (format "%s project %s  %s"
               (zerostack-board--alive-marker (plist-get project :alive))
               (or (plist-get project :name) "")
               (or (plist-get project :path) ""))
       face
       item))
    (dolist (worktree shown-worktrees)
      (zerostack-board--insert-worktree project worktree))
    (when (and (not collapsed) (> (length worktrees) limit))
      (zerostack-board--insert-load-more key (length worktrees) limit))))

(defun zerostack-board--insert-worktree (project worktree)
  "Insert one WORKTREE node and its session children."
  (let* ((branch (plist-get worktree :branch))
         (description (zerostack-board--one-line (plist-get worktree :description)))
         (path (or (plist-get worktree :path) ""))
         (path-marker (zerostack-board--path-marker path))
         (sessions (plist-get worktree :sessions))
         (active (zerostack-board--active-sessions sessions))
         (single-active (and (= (length active) 1) (car active)))
         (key (format "worktree:%s" path))
         (limit (zerostack-board--session-limit key))
         (collapsed (and single-active
                         (not (zerostack-board--session-limit-set-p key))))
         (inline-load-more (and collapsed
                                sessions
                                (list key (length sessions) 0)))
         (label (format "  %s %s  %s  "
                        (zerostack-board--alive-marker (plist-get worktree :alive))
                        (if (and description (not (string-empty-p description)))
                            description
                          "(no description)")
                        (if (and branch (not (string-empty-p branch))) branch "-")))
         (item (append (zerostack-board--worktree-item project worktree)
                       (when single-active
                         (list :session-item (zerostack-board--session-item single-active path))))))
    (zerostack-board--insert-workspace-row
     label
     path-marker
     (format "  %s" path)
     item
     (and single-active (zerostack-board--session-face single-active))
     inline-load-more)
    (unless collapsed
      (zerostack-board--insert-session-list key sessions path nil nil))))

(defun zerostack-board--insert-loose-workspace (workspace)
  "Insert one non-Git WORKSPACE and its session children."
  (let* ((path (or (plist-get workspace :path) ""))
         (path-marker (zerostack-board--path-marker path))
         (sessions (plist-get workspace :sessions))
         (active (zerostack-board--active-sessions sessions))
         (single-active (and (= (length active) 1) (car active)))
         (key (format "workspace:%s" path))
         (limit (zerostack-board--session-limit key))
         (collapsed (and single-active
                         (not (zerostack-board--session-limit-set-p key))))
         (item (append (list :type 'workspace
                             :path path)
                       (when single-active
                         (list :session-item (zerostack-board--session-item single-active path)))))
         (inline-load-more (and collapsed
                                sessions
                                (list key (length sessions) 0))))
    (zerostack-board--insert-workspace-row
     (format "  %s workspace  "
             (zerostack-board--alive-marker (plist-get workspace :alive)))
     path-marker
     (format "  %s" path)
     item
     (and single-active (zerostack-board--session-face single-active))
     inline-load-more)
    (unless collapsed
      (zerostack-board--insert-session-list key sessions path nil nil))))

(defun zerostack-board--active-sessions (sessions)
  "Return live SESSIONS."
  (cl-remove-if-not (lambda (session) (plist-get session :alive)) sessions))

(defun zerostack-board--active-worktrees (worktrees)
  "Return WORKTREES with live sessions."
  (cl-remove-if-not
   (lambda (worktree)
     (zerostack-board--active-sessions (plist-get worktree :sessions)))
   worktrees))

(defun zerostack-board--worktree-face (worktree)
  "Return aggregate face for WORKTREE."
  (when-let ((session (car (zerostack-board--active-sessions (plist-get worktree :sessions)))))
    (zerostack-board--session-face session)))

(defun zerostack-board--worktree-item (project worktree)
  "Return board item metadata for WORKTREE under PROJECT."
  (list :type 'worktree
        :path (or (plist-get worktree :path) "")
        :project-path (plist-get project :path)
        :repo (plist-get project :repo)
        :branch (plist-get worktree :branch)))

(defun zerostack-board--worktree-item-with-session (project worktree)
  "Return WORKTREE item, including its active session when it is singular."
  (let* ((path (or (plist-get worktree :path) ""))
         (active (zerostack-board--active-sessions (plist-get worktree :sessions)))
         (single-active (and (= (length active) 1) (car active))))
    (append (zerostack-board--worktree-item project worktree)
            (when single-active
              (list :session-item (zerostack-board--session-item single-active path))))))

(defun zerostack-board--insert-project-row (text face item load-more)
  "Insert one project row TEXT with FACE, ITEM metadata, and inline LOAD-MORE."
  (let ((start (point)))
    (insert text)
    (add-text-properties
     start (point)
     `(,@(when face `(face ,face))
       mouse-face highlight
       help-echo "RET opens this zerostack board item"
       keymap ,zerostack-board-mode-map
       follow-link t
       zerostack-board-item ,item))
    (insert "  ")
    (apply #'zerostack-board--insert-load-more-inline load-more)
    (insert "\n")))

(defun zerostack-board--insert-workspace-row (prefix name suffix item name-face load-more)
  "Insert a workspace row with NAME optionally styled and LOAD-MORE inline."
  (let ((start (point)))
    (insert prefix name suffix)
    (when name-face
      (add-text-properties start (point) `(face ,name-face)))
    (add-text-properties
     start (point)
     `(mouse-face highlight
                  help-echo "RET opens this zerostack board item"
                  keymap ,zerostack-board-mode-map
                  follow-link t
                  zerostack-board-item ,item))
    (when load-more
      (insert "  ")
      (apply #'zerostack-board--insert-load-more-inline load-more))
    (insert "\n")))

(defun zerostack-board--insert-attention-session (session)
  "Insert one SESSION that needs attention."
  (let* ((cwd (or (plist-get session :cwd) ""))
         (title (zerostack-board--one-line (plist-get session :title)))
         (display-title (if (string-empty-p title) "(untitled)" title))
         (item (plist-put (zerostack-board--session-item session cwd) :attention t))
         (start (point)))
    (insert (format "    %s  %s  " display-title cwd))
    (add-text-properties
     start (point)
     `(mouse-face highlight
                  help-echo "RET opens this zerostack session"
                  keymap ,zerostack-board-mode-map
                  follow-link t
                  zerostack-board-item ,item))
    (zerostack-board--insert-dismiss-button session)
    (insert "\n")))

(defun zerostack-board--insert-dismiss-button (session)
  "Insert a dismiss button for SESSION."
  (insert-text-button "dismiss"
                      'action (lambda (_)
                                (zerostack-board--dismiss-attention session))
                      'follow-link t
                      'help-echo "Remove this session from Needs attention"))

(defun zerostack-board--dismiss-attention (session)
  "Dismiss SESSION from the Needs attention section."
  (let ((id (plist-get session :id)))
    (unless id
      (user-error "Session has no id"))
    (with-temp-buffer
      (let ((status (call-process zerostack-command nil t nil "--emacs-dismiss-attention" id)))
        (unless (zerop status)
          (user-error "zerostack --emacs-dismiss-attention exited with %s: %s"
                      status
                      (string-trim (buffer-string))))))
    (zerostack-board-refresh)))

(defun zerostack-board--insert-session-list (key sessions worktree-path &optional suppress-load-more subdued-session-id)
  "Insert paginated SESSIONS for KEY under WORKTREE-PATH."
  (let* ((limit (zerostack-board--session-limit key))
         (shown (cl-subseq sessions 0 (min limit (length sessions)))))
    (dolist (session shown)
      (unless (equal (plist-get session :id) subdued-session-id)
        (zerostack-board--insert-session session worktree-path)))
    (when (and (not suppress-load-more) (> (length sessions) limit))
      (zerostack-board--insert-load-more key (length sessions) limit))))

(defun zerostack-board--session-limit-set-p (key)
  "Return non-nil when KEY has an explicit board session limit."
  (and zerostack-board--session-limits
       (gethash key zerostack-board--session-limits)))

(defun zerostack-board--session-limit (key)
  "Return currently visible session count for board list KEY."
  (or (and zerostack-board--session-limits
           (gethash key zerostack-board--session-limits))
      5))

(defun zerostack-board--set-session-limit (key limit)
  "Set visible session count for board list KEY to LIMIT."
  (unless zerostack-board--session-limits
    (setq-local zerostack-board--session-limits (make-hash-table :test 'equal)))
  (puthash key limit zerostack-board--session-limits))

(defun zerostack-board--insert-load-more (key total shown)
  "Insert a load-more row for session list KEY."
  (let ((remaining (- total shown)))
    (zerostack-board--insert-row
     (format "    + show 5 more (%d remaining)" remaining)
     'zerostack-link-face
     (list :type 'load-more
           :key key
           :total total
           :shown shown))))

(defun zerostack-board--insert-load-more-inline (key total shown)
  "Insert an inline load-more button for session list KEY."
  (let* ((remaining (- total shown))
         (start (point))
         (item (list :type 'load-more
                     :key key
                     :total total
                     :shown shown)))
    (insert (format "+ show 5 more (%d remaining)" remaining))
    (add-text-properties
     start (point)
     `(face zerostack-link-face
            mouse-face highlight
            help-echo "RET shows more zerostack sessions"
            keymap ,zerostack-board-mode-map
            follow-link t
            zerostack-board-item ,item))))

(defun zerostack-board--session-item (session worktree-path)
  "Return board item metadata for SESSION under WORKTREE-PATH."
  (list :type 'session
        :id (plist-get session :id)
        :title (zerostack-board--one-line (plist-get session :title))
        :cwd (plist-get session :cwd)
        :worktree-path worktree-path
        :pid (plist-get session :pid)
        :socket (plist-get session :socket)
        :alive (plist-get session :alive)))

(defun zerostack-board--insert-session (session &optional worktree-path subdued-session-id)
  "Insert one SESSION node."
  (let* ((alive (plist-get session :alive))
         (title (zerostack-board--one-line (plist-get session :title)))
         (display-title (if (string-empty-p title) "(untitled)" title))
         (updated-at (or (plist-get session :updated-at) ""))
         (age (zerostack-board--relative-time updated-at))
         (buffer (zerostack--find-chat-buffer (plist-get session :id)
                                              (plist-get session :socket)))
         (usage (zerostack-board--session-usage session buffer))
         (face (if (equal (plist-get session :id) subdued-session-id)
                   'zerostack-board-session-face
                 (zerostack-board--session-face session buffer)))
         (item (zerostack-board--session-item session worktree-path))
         (start (point)))
    (insert (format "    %s " (zerostack-board--alive-marker alive)))
    (let ((title-start (point)))
      (insert display-title)
      (add-text-properties title-start (point) `(face ,face)))
    (insert (format "  %s%s" age (if usage (format "  %s" usage) "")))
    (add-text-properties
     start (point)
     `(mouse-face highlight
                  help-echo "RET opens this zerostack board item"
                  keymap ,zerostack-board-mode-map
                  follow-link t
                  zerostack-board-item ,item))
    (insert "\n")))

(defun zerostack-board--session-face (session &optional buffer)
  "Return board face for SESSION, considering live BUFFER state."
  (let* ((alive (plist-get session :alive))
         (chat-buffer (or buffer
                          (zerostack--find-chat-buffer (plist-get session :id)
                                                       (plist-get session :socket))))
         (needs-input (and chat-buffer
                           (with-current-buffer chat-buffer
                             (zerostack--needs-input-p))))
         (thinking (and chat-buffer
                        (with-current-buffer chat-buffer zerostack--thinking))))
    (cond
     (needs-input 'zerostack-board-input-face)
     (thinking 'zerostack-board-thinking-face)
     (alive 'zerostack-board-alive-face)
     (t 'zerostack-board-session-face))))

(defun zerostack-board--insert-row (text face item)
  "Insert one board row TEXT with FACE and clickable ITEM metadata."
  (let ((start (point)))
    (insert text)
    (add-text-properties
     start (point)
     `(,@(when face `(face ,face))
       mouse-face highlight
       help-echo "RET opens this zerostack board item"
       keymap ,zerostack-board-mode-map
       follow-link t
       zerostack-board-item ,item))
    (insert "\n")))

(defun zerostack-board--alive-marker (alive)
  "Return the board marker for ALIVE state."
  (if alive "*" " "))

(defun zerostack-board--session-usage (session buffer)
  "Return context usage text for SESSION, preferring live BUFFER state."
  (let* ((tokens (or (and buffer (with-current-buffer buffer zerostack--tokens))
                     (plist-get session :tokens)))
         (window (or (and buffer (with-current-buffer buffer zerostack--context-window))
                     (plist-get session :context-window))))
    (zerostack--format-token-usage tokens window)))

(defun zerostack--format-token-usage (tokens window)
  "Return TUI-style token usage for TOKENS and WINDOW."
  (when (and (numberp tokens) (numberp window) (> window 0))
    (format "(%s/%s%%)" (zerostack--format-token-count tokens) (/ (* tokens 100) window))))

(defun zerostack--format-token-count (tokens)
  "Return compact token count string for TOKENS."
  (cond
   ((>= tokens 1000000) (format "%.1fM" (/ tokens 1000000.0)))
   ((>= tokens 1000) (format "%dk" (/ tokens 1000)))
   (t (format "%s" tokens))))

(defun zerostack-board--current-directory-root ()
  "Return the current Projectile/project root, or `default-directory'."
  (zerostack-board--normalize-directory
   (or (and (fboundp 'projectile-project-root)
            (ignore-errors (projectile-project-root)))
       (and (fboundp 'project-current)
            (when-let ((project (project-current nil)))
              (car (project-roots project))))
       default-directory)))

(defun zerostack-board--normalize-directory (directory)
  "Return canonical DIRECTORY without a trailing slash."
  (directory-file-name
   (if (and directory (file-directory-p directory))
       (file-truename directory)
     (expand-file-name (or directory default-directory)))))

(defun zerostack-board--remember-directory (directory)
  "Remember DIRECTORY as an Emacs-side board pin.

Return non-nil when DIRECTORY was newly added."
  (let* ((dir (zerostack-board--normalize-directory directory))
         (existing (mapcar #'zerostack-board--normalize-directory
                           zerostack-board-directories)))
    (unless (member dir existing)
      (setq zerostack-board-directories
            (append zerostack-board-directories (list dir)))
      t)))

(defun zerostack-board--snapshot-has-directory-p (snapshot directory)
  "Return non-nil if SNAPSHOT already contains DIRECTORY."
  (let ((key (zerostack-board--normalize-directory directory))
        (keys (zerostack-board--snapshot-directory-keys snapshot)))
    (gethash key keys)))

(defun zerostack-board--snapshot-directory-keys (snapshot)
  "Return hash table of normalized directories visible in board SNAPSHOT."
  (let ((keys (make-hash-table :test 'equal))
        (projects (plist-get (cdr snapshot) :projects))
        (workspaces (plist-get (cdr snapshot) :loose-workspaces)))
    (cl-labels ((add (path)
                  (when (and (stringp path) (not (string-empty-p path)))
                    (puthash (zerostack-board--normalize-directory path) t keys)))
                (add-session (session)
                  (add (plist-get session :cwd))))
      (dolist (project projects)
        (add (plist-get project :path))
        (dolist (worktree (plist-get project :worktrees))
          (add (plist-get worktree :path))
          (dolist (session (plist-get worktree :sessions))
            (add-session session))))
      (dolist (workspace workspaces)
        (add (plist-get workspace :path))
        (dolist (session (plist-get workspace :sessions))
          (add-session session))))
    keys))

(defun zerostack-board--pinned-workspaces (snapshot)
  "Return pinned workspace plists not already present in SNAPSHOT."
  (let ((keys (zerostack-board--snapshot-directory-keys snapshot))
        result)
    (dolist (directory zerostack-board-directories)
      (let ((dir (zerostack-board--normalize-directory directory)))
        (unless (gethash dir keys)
          (puthash dir t keys)
          (push (list :path dir
                      :alive nil
                      :updated-at ""
                      :sessions nil
                      :pinned t)
                result))))
    (nreverse result)))

(defun zerostack-board--one-line (value)
  "Return VALUE as compact one-line text for board rows."
  (string-trim
   (replace-regexp-in-string "[[:space:]]+" " " (or value ""))))

(defun zerostack-board--path-marker (path)
  "Return a compact directory marker for board PATH."
  (if (and path (not (string-empty-p path)))
      (concat (file-name-nondirectory (directory-file-name path)) "/")
    "-/"))

(defun zerostack-board--relative-time (timestamp)
  "Return a compact relative age for TIMESTAMP."
  (condition-case nil
      (let* ((seconds (max 0 (floor (float-time
                                     (time-subtract (current-time)
                                                    (date-to-time timestamp)))))))
        (cond
         ((< seconds 60) "just now")
         ((< seconds 3600) (format "%dm ago" (/ seconds 60)))
         ((< seconds 86400) (format "%dh ago" (/ seconds 3600)))
         ((< seconds 2592000) (format "%dd ago" (/ seconds 86400)))
         ((< seconds 31536000) (format "%dmo ago" (/ seconds 2592000)))
         (t (format "%dy ago" (/ seconds 31536000)))))
    (error "unknown age")))

(defun zerostack-board--item-at-point (&optional event)
  "Return board item at point or mouse EVENT."
  (when (eventp event)
    (posn-set-point (event-end event)))
  (get-text-property (point) 'zerostack-board-item))

(defun zerostack-board--candidate-label (item line)
  "Return completion label for board ITEM rendered on LINE."
  (let ((type (plist-get item :type)))
    (format "%s: %s"
            (pcase type
              ('project "project")
              ('worktree "worktree")
              ('workspace "workspace")
              ('session (if (plist-get item :attention) "attention" "session"))
              ('load-more "more")
              (_ (format "%s" type)))
            (string-trim line))))

(defun zerostack-board--candidates (&optional predicate)
  "Return visible board completion candidates matching PREDICATE."
  (let (candidates)
    (save-excursion
      (goto-char (point-min))
      (while (< (point) (point-max))
        (let* ((start (point))
               (end (line-end-position))
               (item (or (get-text-property start 'zerostack-board-item)
                         (and (< start end)
                              (get-text-property (1+ start) 'zerostack-board-item)))))
          (when (and item (or (not predicate) (funcall predicate item)))
            (let* ((line (buffer-substring-no-properties start end))
                   (label (zerostack-board--candidate-label item line)))
              (push (cons label (copy-marker start)) candidates)))
          (forward-line 1))))
    (nreverse candidates)))

(defun zerostack-board--read-candidate (prompt &optional predicate)
  "Read a visible board candidate with PROMPT matching PREDICATE."
  (let ((candidates (zerostack-board--candidates predicate)))
    (unless candidates
      (user-error "No matching zerostack board items"))
    (cdr (assoc (completing-read prompt candidates nil t) candidates))))

(defun zerostack-board-jump ()
  "Jump to a visible zerostack board item using completion."
  (interactive)
  (goto-char (zerostack-board--read-candidate "Jump to: ")))

(defun zerostack-board-open ()
  "Open a visible zerostack board item selected with completion."
  (interactive)
  (goto-char (zerostack-board--read-candidate "Open: "))
  (zerostack-board-open-at-point))

(defun zerostack-board-open-attention ()
  "Dismiss and open a visible Needs attention session selected with completion."
  (interactive)
  (let* ((marker (zerostack-board--read-candidate
                  "Open attention: "
                  (lambda (item)
                    (and (eq (plist-get item :type) 'session)
                         (plist-get item :attention)))))
         (item (save-excursion
                 (goto-char marker)
                 (zerostack-board--item-at-point))))
    (zerostack-board--dismiss-attention item)
    (zerostack-board--open-session item)))

(defun zerostack-board-open-at-point (&optional event)
  "Open the board item at point or mouse EVENT."
  (interactive (list last-nonmenu-event))
  (let ((item (zerostack-board--item-at-point event)))
    (pcase (plist-get item :type)
      ('project
       (if-let ((workspace (plist-get item :workspace-item)))
           (zerostack-board-open-at-point-for-item workspace)
         (dired (plist-get item :path))))
      ('worktree
       (if-let ((session (plist-get item :session-item)))
           (zerostack-board--open-session session)
         (dired (plist-get item :path))))
      ('workspace
       (if-let ((session (plist-get item :session-item)))
           (zerostack-board--open-session session)
         (dired (plist-get item :path))))
      ('session
       (zerostack-board--open-session item))

      ('load-more
       (zerostack-board--load-more item))
      (_
       (message "No zerostack board item at point")))))

(defun zerostack-board-open-at-point-for-item (item)
  "Open board ITEM."
  (pcase (plist-get item :type)
    ('worktree
     (if-let ((session (plist-get item :session-item)))
         (zerostack-board--open-session session)
       (dired (plist-get item :path))))
    ('workspace
     (if-let ((session (plist-get item :session-item)))
         (zerostack-board--open-session session)
       (dired (plist-get item :path))))
    ('session
     (zerostack-board--open-session item))
    (_
     (dired (plist-get item :path)))))

(defun zerostack-board--open-session (item)
  "Open SESSION board ITEM."
  (if-let ((socket (plist-get item :socket)))
      (zerostack-connect socket
                         (plist-get item :title)
                         (plist-get item :cwd)
                         (plist-get item :worktree-path)
                         (plist-get item :id))
    (let ((default-directory (file-name-as-directory
                              (or (plist-get item :cwd) default-directory))))
      (zerostack (list "--session" (plist-get item :id))
                 (plist-get item :title)
                 (plist-get item :cwd)
                 (plist-get item :worktree-path)
                 (plist-get item :id)))))

(defun zerostack-board--load-more (item)
  "Show five more sessions for load-more ITEM."
  (let ((key (plist-get item :key))
        (shown (or (plist-get item :shown) 0))
        (total (or (plist-get item :total) 0))
        (line (line-number-at-pos))
        (column (current-column)))
    (zerostack-board--set-session-limit key (min total (max 5 (+ shown 5))))
    (when zerostack-board--snapshot
      (zerostack-board--render zerostack-board--snapshot)
      (zerostack--goto-line-column line column))))

(defun zerostack-board-create-at-point (&optional event)
  "Create a board child for the project or worktree at point."
  (interactive (list last-nonmenu-event))
  (let ((item (zerostack-board--item-at-point event)))
    (pcase (plist-get item :type)
      ('project
       (zerostack-board--create-worktree item))
      ('worktree
       (zerostack-board--create-session item))
      ('workspace
       (zerostack-board--create-session item))
      (_
       (message "Press c on a project to create a worktree, or a worktree/workspace to create a session")))))

(defun zerostack-board-set-description-at-point (&optional event)
  "Set the Git branch description for the worktree at point."
  (interactive (list last-nonmenu-event))
  (let ((item (zerostack-board--item-at-point event)))
    (pcase (plist-get item :type)
      ('worktree
       (zerostack-board--set-worktree-description item))
      (_
       (message "Press d on a worktree to set its branch description")))))

(defun zerostack-board--set-worktree-description (worktree)
  "Set branch description for WORKTREE."
  (let ((project-path (plist-get worktree :project-path))
        (branch (plist-get worktree :branch)))
    (unless (and branch (not (string-empty-p branch)) (not (equal branch "detached")))
      (user-error "Worktree has no branch"))
    (let ((description (string-trim (read-string "Branch description: "))))
      (zerostack-board--call-git project-path "config"
                                 (format "branch.%s.description" branch)
                                 description)
      (message "Set branch description for %s" branch)
      (zerostack-board-refresh))))

(defun zerostack-board-stop-at-point (&optional event)
  "Stop the live zerostack session process at point."
  (interactive (list last-nonmenu-event))
  (let ((item (zerostack-board--item-at-point event)))
    (pcase (plist-get item :type)
      ('session
       (if (plist-get item :attention)
           (zerostack-board--dismiss-attention item)
         (zerostack-board--stop-session item)))
      ((or 'worktree 'workspace)
       (if-let ((session (plist-get item :session-item)))
           (zerostack-board--stop-session session)
         (message "Press s on a live session to stop its process")))
      (_
       (message "Press s on a live session to stop its process")))))

(defun zerostack-board-trash-at-point (&optional event)
  "Move the worktree or session at point to trash after confirmation."
  (interactive (list last-nonmenu-event))
  (let ((item (zerostack-board--item-at-point event)))
    (pcase (plist-get item :type)
      ('worktree
       (let ((path (plist-get item :path)))
         (when (yes-or-no-p (format "Move worktree %s to trash? " path))
           (zerostack-board--trash-worktree item))))
      ('session
       (if (plist-get item :attention)
           (zerostack-board--dismiss-attention item)
         (let ((title (or (plist-get item :title) (plist-get item :id))))
           (when (yes-or-no-p (format "Move session %s to trash? " title))
             (zerostack-board--trash-session item)))))
      (_
       (message "Press x on a worktree or session to move it to trash")))))

(defun zerostack-board--stop-session (session)
  "Stop a live SESSION process, close its chat buffer, and refresh the board."
  (let ((pid (plist-get session :pid))
        (title (or (plist-get session :title) (plist-get session :id))))
    (unless (plist-get session :alive)
      (user-error "Session is not alive: %s" title))
    (unless (integerp pid)
      (user-error "Live session has no process id: %s" title))
    (when (yes-or-no-p (format "Stop zerostack process %s for %s? " pid title))
      (signal-process pid 'term)
      (when-let ((chat-buffer (zerostack--find-chat-buffer (plist-get session :id)
                                                            (plist-get session :socket))))
        (kill-buffer chat-buffer))
      (message "Stopped zerostack process %s" pid)
      (run-at-time 0.2 nil
                   (lambda (buffer)
                     (when (buffer-live-p buffer)
                       (with-current-buffer buffer
                         (zerostack-board-refresh))))
                   (current-buffer)))))

(defun zerostack-board--create-worktree (project)
  "Create a git worktree under PROJECT using Emacs-side Git commands."
  (let* ((project-path (plist-get project :path))
         (branch (string-trim (read-string "New branch: "))))
    (when (string-empty-p branch)
      (user-error "Branch name is required"))
    (let* ((base (file-name-directory (directory-file-name project-path)))
           (dir-name (zerostack-board--safe-dir-name branch))
           (default-path (expand-file-name dir-name base))
           (path (expand-file-name
                  (read-file-name "Worktree path: " base default-path nil dir-name)))
           (description (string-trim (read-string "Branch description: "))))
      (zerostack-board--call-git project-path "worktree" "add" "-b" branch path)
      (unless (string-empty-p description)
        (zerostack-board--call-git project-path "config"
                                   (format "branch.%s.description" branch)
                                   description))
      (message "Created worktree %s" path)
      (zerostack-board-refresh))))

(defun zerostack-board--create-session (worktree)
  "Start a new zerostack session in WORKTREE."
  (let ((path (plist-get worktree :path)))
    (unless (and path (file-directory-p path))
      (user-error "Worktree does not exist: %s" path))
    (let ((default-directory (file-name-as-directory path)))
      (zerostack nil))))

(defun zerostack-board--trash-worktree (worktree)
  "Move WORKTREE path to trash and prune Git's missing-worktree metadata."
  (let ((path (plist-get worktree :path))
        (project-path (plist-get worktree :project-path)))
    (unless (and path (file-directory-p path))
      (user-error "Worktree does not exist: %s" path))
    (zerostack-board--trash-path path)
    (when (and project-path (file-directory-p project-path))
      (ignore-errors
        (zerostack-board--call-git project-path "worktree" "prune")))
    (message "Moved worktree to trash: %s" path)
    (zerostack-board-refresh)))

(defun zerostack-board--trash-session (session)
  "Move SESSION's persisted JSON file to trash."
  (let* ((id (plist-get session :id))
         (path (zerostack-board--session-file id)))
    (unless (and path (file-exists-p path))
      (user-error "Session file does not exist: %s" path))
    (zerostack-board--trash-path path)
    (message "Moved session to trash: %s" id)
    (zerostack-board-refresh)))

(defun zerostack-board--safe-dir-name (value)
  "Return VALUE sanitized for use as a worktree directory name."
  (let ((name (replace-regexp-in-string "[^[:alnum:]_.-]+" "-" value)))
    (if (string-empty-p name) "worktree" name)))

(defun zerostack-board--call-git (dir &rest args)
  "Run git -C DIR ARGS, returning stdout or signaling a user error."
  (with-temp-buffer
    (let ((status (apply #'process-file "git" nil t nil "-C" dir args)))
      (unless (and (integerp status) (zerop status))
        (user-error "git %s failed in %s: %s"
                    (string-join args " ")
                    dir
                    (string-trim (buffer-string))))
      (buffer-string))))

(defun zerostack-board--data-dir ()
  "Return the data directory used by the current Emacs environment."
  (file-name-as-directory
   (or (getenv "ZS_DATA_DIR")
       (expand-file-name "zerostack"
                         (or (getenv "XDG_DATA_HOME")
                             "~/.local/share")))))

(defun zerostack-board--session-file (id)
  "Return the persisted session JSON file path for ID."
  (when id
    (expand-file-name (concat id ".json")
                      (expand-file-name "sessions" (zerostack-board--data-dir)))))

(defun zerostack-board--trash-path (path)
  "Move PATH to the user's trash, or a zerostack trash fallback."
  (if (fboundp 'move-file-to-trash)
      (move-file-to-trash path)
    (let* ((trash-dir (expand-file-name "trash" (zerostack-board--data-dir)))
           (target (zerostack-board--trash-target trash-dir path)))
      (make-directory trash-dir t)
      (rename-file path target))))

(defun zerostack-board--trash-target (trash-dir path)
  "Return a unique fallback trash target under TRASH-DIR for PATH."
  (let* ((base (file-name-nondirectory (directory-file-name path)))
         (stamp (format-time-string "%Y%m%d%H%M%S"))
         (target (expand-file-name (format "%s-%s" base stamp) trash-dir))
         (n 0))
    (while (file-exists-p target)
      (setq n (1+ n))
      (setq target (expand-file-name (format "%s-%s-%d" base stamp n) trash-dir)))
    target))

(defun zerostack--start-server (args)
  "Start a zerostack server with ARGS in the current buffer."
  (setq zerostack--server-args args)
  (let* ((client-buffer (current-buffer))
         (stderr-buffer (generate-new-buffer " *zerostack stderr*"))
         (command (append (list zerostack-command "--emacs") args))
         (process (make-process
                   :name "zerostack-server"
                   :buffer nil
                   :command command
                   :stderr stderr-buffer
                   :noquery t
                   :sentinel #'zerostack--server-sentinel)))
    (process-put process 'zerostack-buffer client-buffer)
    (process-put process 'zerostack-stderr-buffer stderr-buffer)
    (setq zerostack--server-process process)
    (setq zerostack--startup-timer
          (run-at-time 0.05 0.05 #'zerostack--poll-server-startup
                       client-buffer stderr-buffer))))

(defun zerostack--poll-server-startup (client-buffer stderr-buffer)
  "Poll STDERR-BUFFER until CLIENT-BUFFER can connect to the printed socket."
  (unless (and (buffer-live-p client-buffer) (buffer-live-p stderr-buffer))
    (when (buffer-live-p client-buffer)
      (with-current-buffer client-buffer
        (when zerostack--startup-timer
          (cancel-timer zerostack--startup-timer)
          (setq zerostack--startup-timer nil)))))
  (when (and (buffer-live-p client-buffer) (buffer-live-p stderr-buffer))
    (with-current-buffer stderr-buffer
      (when (save-excursion
              (goto-char (point-min))
              (re-search-forward "^socket \\(.*\\)$" nil t))
        (let ((socket (match-string 1)))
          (with-current-buffer client-buffer
            (when zerostack--startup-timer
              (cancel-timer zerostack--startup-timer)
              (setq zerostack--startup-timer nil))
            (zerostack--connect-buffer socket)))))))

(defun zerostack--server-sentinel (process event)
  "Record zerostack server PROCESS EVENT in its client buffer when possible."
  (let ((buffer (process-get process 'zerostack-buffer))
        (stderr-text (zerostack--server-stderr-text process))
        (terminal (memq (process-status process) '(exit signal closed failed))))
    (when (buffer-live-p buffer)
      (with-current-buffer buffer
        (when (and terminal (eq process zerostack--server-process))
          (when zerostack--startup-timer
            (cancel-timer zerostack--startup-timer)
            (setq zerostack--startup-timer nil))
          (setq zerostack--server-process nil))
        (zerostack--append-local-line
         (if (and (not (zerop (process-exit-status process)))
                  stderr-text
                  (not (string-empty-p stderr-text)))
             (format "server %s: %s" (string-trim event) stderr-text)
           (format "server %s" (string-trim event)))
         (if (zerop (process-exit-status process)) 'zs-muted 'zs-error))))))

(defun zerostack--server-stderr-text (process)
  "Return user-facing stderr text recorded for server PROCESS."
  (let ((stderr-buffer (process-get process 'zerostack-stderr-buffer)))
    (when (buffer-live-p stderr-buffer)
      (with-current-buffer stderr-buffer
        (let* ((lines (split-string (buffer-string) "\n" t "[[:space:]]+"))
               (visible-lines
                (cl-remove-if
                 (lambda (line)
                   (string-match-p "\\`socket[[:space:]]+" line))
                 lines)))
          (string-join visible-lines "\n"))))))

(defun zerostack--connect-buffer (socket)
  "Connect the current zerostack buffer to SOCKET."
  (setq zerostack--socket (zerostack--normalize-socket socket))
  (let ((proc (make-network-process
               :name "zerostack"
               :buffer (current-buffer)
               :family 'local
               :service zerostack--socket
               :coding 'utf-8-unix
               :filter #'zerostack--process-filter
               :sentinel #'zerostack--process-sentinel
               :noquery t)))
    (setq zerostack--process proc)
    (when zerostack--server-process
      (process-put zerostack--server-process 'zerostack-buffer (current-buffer)))
    (zerostack-send-hello)
    (zerostack-attach)
    (zerostack--request-metadata)))

(defun zerostack--request-metadata ()
  "Request session metadata for local buffer naming without user status noise."
  (let ((request (zerostack--next-request)))
    (setq zerostack--metadata-status-request request)
    (zerostack--send-command 'status :request request)))

(defun zerostack--process-sentinel (process event)
  "Record zerostack socket PROCESS EVENT."
  (when (buffer-live-p (process-buffer process))
    (with-current-buffer (process-buffer process)
      (zerostack--append-local-line
       (format "connection %s" (string-trim event))
       (if (memq (process-status process) '(closed exit signal failed)) 'zs-error 'zs-muted)))))

(defun zerostack--process-filter (process chunk)
  "Handle protocol CHUNK from PROCESS."
  (when (buffer-live-p (process-buffer process))
    (with-current-buffer (process-buffer process)
      (zerostack--consume-chunk chunk))))

(defun zerostack--consume-chunk (chunk)
  "Consume one raw protocol CHUNK in the current buffer."
  (setq zerostack--line-buffer (concat zerostack--line-buffer chunk))
  (let ((start 0))
    (while (string-match "\n" zerostack--line-buffer start)
      (let ((line (substring zerostack--line-buffer 0 (match-beginning 0))))
        (setq zerostack--line-buffer
              (substring zerostack--line-buffer (match-end 0)))
        (setq start 0)
        (unless (string-empty-p (string-trim line))
          (condition-case err
              (zerostack--handle-form (car (read-from-string line)))
            (error
             (zerostack--append-local-line
              (format "protocol parse error: %s" (error-message-string err))
              'zs-error))))))))

(defun zerostack--send-form (form)
  "Send FORM as one protocol line and return the encoded line."
  (let ((line (concat (let ((print-escape-newlines t))
                        (prin1-to-string form))
                      "\n")))
    (cond
     (zerostack--send-function
      (funcall zerostack--send-function line))
     ((and zerostack--process (process-live-p zerostack--process))
      (process-send-string zerostack--process line))
     (t
      (zerostack--append-local-line "not connected" 'zs-error)))
    line))

(defun zerostack--next-request ()
  "Return the next request id for commands that support one."
  (cl-incf zerostack--request-counter))

(defun zerostack--send-command (name &rest fields)
  "Send command NAME with plist FIELDS."
  (zerostack--send-form (cons name fields)))

(defun zerostack-send-hello ()
  "Send a protocol hello command."
  (interactive)
  (zerostack--send-command 'hello
                           :request (zerostack--next-request)
                           :protocol 1
                           :cols zerostack--cols))

(defun zerostack-attach ()
  "Request a full rendered session snapshot."
  (interactive)
  (zerostack--send-command 'attach
                           :request (zerostack--next-request)
                           :cols zerostack--cols))

(defun zerostack-render ()
  "Request a full rendered session snapshot via the render alias."
  (interactive)
  (zerostack--send-command 'render
                           :request (zerostack--next-request)
                           :cols zerostack--cols))

(defun zerostack-set-view (cols)
  "Set server render width to COLS."
  (interactive "nColumns: ")
  (setq zerostack--cols (max 20 cols))
  (zerostack--send-command 'set-view
                           :request (zerostack--next-request)
                           :cols zerostack--cols))

(defun zerostack-provider-menu (provider)
  "Switch the current zerostack session to PROVIDER."
  (interactive (list (zerostack--read-provider "Session provider: " zerostack--provider)))
  (zerostack--send-command 'provider
                           :request (zerostack--next-request)
                           :provider provider))

(defun zerostack-model-menu (model)
  "Switch the current zerostack session to MODEL."
  (interactive
   (list (zerostack--read-model zerostack--provider zerostack--model)))
  (zerostack--send-command 'model
                           :request (zerostack--next-request)
                           :model model))

(defun zerostack-subagent-provider-menu (provider)
  "Switch the current zerostack session's subagent provider to PROVIDER."
  (interactive (list (zerostack--read-provider "Session subagent provider: " zerostack--subagent-provider)))
  (zerostack--send-command 'subagent-provider
                           :request (zerostack--next-request)
                           :provider provider))

(defun zerostack-subagent-model-menu (model)
  "Switch the current zerostack session's subagent model to MODEL."
  (interactive
   (let* ((provider (or zerostack--subagent-provider zerostack--provider))
          (model (zerostack--read-model provider zerostack--subagent-model)))
     (list model)))
  (zerostack--send-command 'subagent-model
                           :request (zerostack--next-request)
                           :model model))

(defun zerostack-list-tools ()
  "List built-in zerostack tools exposed to the current session."
  (interactive)
  (zerostack--send-command 'list-tools :request (zerostack--next-request)))

(defun zerostack-tools ()
  "Alias for `zerostack-list-tools'."
  (interactive)
  (zerostack-list-tools))

(defun zerostack-goal (&optional clear)
  "Show current goal evaluator state. With CLEAR, clear the goal."
  (interactive "P")
  (zerostack--send-command 'goal
                           :request (zerostack--next-request)
                           :action (if clear 'clear 'show)))

(defun zerostack-clear-goal ()
  "Clear the current goal evaluator state."
  (interactive)
  (zerostack-goal t))

(defun zerostack-mcp ()
  "List configured MCP servers and tools."
  (interactive)
  (zerostack--send-command 'mcp :request (zerostack--next-request)))

(defun zerostack--reasoning-effort-supported-p ()
  "Return non-nil when the current model supports OpenAI reasoning effort."
  (and (boundp 'zerostack--reasoning-effort-supported)
       zerostack--reasoning-effort-supported))

(defun zerostack--reasoning-effort-options ()
  "Return server-advertised reasoning efforts, with an old-server fallback."
  (or zerostack--reasoning-efforts '("minimal" "low" "medium" "high")))

(defun zerostack-thinking-menu (&optional level)
  "Set native zerostack thinking/reasoning LEVEL."
  (interactive)
  (let* ((choices (append '("on" "off")
                          (when (zerostack--reasoning-effort-supported-p)
                            (zerostack--reasoning-effort-options))))
         (level (or level
                    (completing-read "Thinking: " choices nil t nil nil zerostack--thinking-level))))
    (zerostack--send-command 'thinking
                             :request (zerostack--next-request)
                             :level level)))

(defun zerostack-send-prompt (text)
  "Send TEXT as a zerostack prompt."
  (interactive "sPrompt: ")
  (zerostack--set-thinking t)
  (zerostack--send-command 'prompt
                           :request (zerostack--next-request)
                           :text text))

(defun zerostack--message-index-at-point ()
  "Return rendered zerostack message index at point."
  (get-text-property (point) 'zerostack-message-index))

(defun zerostack-rewind (&optional index)
  "Rewind the current session to before message INDEX and load it for editing."
  (interactive)
  (let* ((index (or index
                    (and (use-region-p)
                         (save-excursion
                           (goto-char (region-beginning))
                           (zerostack--message-index-at-point)))
                    (zerostack--message-index-at-point)
                    (read-number "Rewind to user message index: ")))
         (request (zerostack--next-request)))
    (unless (yes-or-no-p (format "Rewind to before message %s? " index))
      (user-error "Rewind cancelled"))
    (zerostack--set-status (format "rewinding to message %s" index))
    (zerostack--send-command 'rewind :request request :index index)))

(defun zerostack-redo ()
  "Restore the last Emacs/TUI rewind or undo."
  (interactive)
  (zerostack--send-command 'redo :request (zerostack--next-request)))

(defun zerostack-compact (&optional instructions)
  "Compact the current zerostack session, optionally using INSTRUCTIONS."
  (interactive "sCompaction instructions (optional): ")
  (let ((request (zerostack--next-request))
        (instructions (and instructions (string-trim instructions))))
    (zerostack--set-status "compacting...")
    (zerostack--set-thinking t)
    (if (and instructions (not (string-empty-p instructions)))
        (zerostack--send-command 'compact
                                 :request request
                                 :instructions instructions)
      (zerostack--send-command 'compact :request request))))

(defun zerostack-loop ()
  "Start a loop, or stop the active loop in the current zerostack session."
  (interactive)
  (if zerostack--loop-active
      (when (yes-or-no-p "Stop active zerostack loop? ")
        (zerostack-loop-stop))
    (let* ((prompt (string-trim (read-string "Loop prompt: ")))
           (max-text (string-trim (read-string "Max iterations (empty for unlimited): ")))
           (run (string-trim (read-string "Validation command (optional): ")))
           (max (unless (string-empty-p max-text)
                  (string-to-number max-text))))
      (when (string-empty-p prompt)
        (user-error "Loop prompt is required"))
      (zerostack-loop-start prompt max run))))

(defun zerostack-loop-start (prompt &optional max run plan)
  "Start a native loop with PROMPT and optional MAX/RUN/PLAN settings."
  (interactive "sLoop prompt: ")
  (let ((request (zerostack--next-request))
        (fields (list :request nil :prompt prompt)))
    (setq fields (plist-put fields :request request))
    (when (and max (numberp max) (> max 0))
      (setq fields (plist-put fields :max max)))
    (when (and run (not (string-empty-p (string-trim run))))
      (setq fields (plist-put fields :run (string-trim run))))
    (when (and plan (not (string-empty-p (string-trim plan))))
      (setq fields (plist-put fields :plan (string-trim plan))))
    (zerostack--set-thinking t)
    (zerostack--set-status "starting loop")
    (apply #'zerostack--send-command 'loop-start fields)))

(defun zerostack-loop-stop ()
  "Stop the native loop and abort the active loop iteration if needed."
  (interactive)
  (setq zerostack--loop-active nil
        zerostack--loop-label nil)
  (zerostack--clear-pending-permissions)
  (zerostack--set-thinking nil)
  (zerostack--set-status "stopping loop")
  (zerostack--send-command 'loop-stop :request (zerostack--next-request)))

(defun zerostack-loop-status ()
  "Request native loop status."
  (interactive)
  (zerostack--send-command 'loop-status :request (zerostack--next-request)))

(defun zerostack-abort ()
  "Abort the active zerostack turn."
  (interactive)
  (zerostack--clear-pending-permissions)
  (zerostack--set-thinking nil)
  (zerostack--set-status nil)
  (zerostack--send-command 'abort :request (zerostack--next-request)))

(defun zerostack-request-sessions (&optional limit)
  "Request live zerostack sessions from the connected server.

LIMIT defaults to 50."
  (interactive "P")
  (zerostack--send-command 'list-sessions
                           :request (zerostack--next-request)
                           :limit (or (and limit (prefix-numeric-value limit)) 50)))

(defun zerostack-request-status ()
  "Request current session status."
  (interactive)
  (zerostack--send-command 'status :request (zerostack--next-request)))

(defun zerostack-attachment-menu ()
  "Choose an attachment action for the current zerostack session."
  (interactive)
  (let* ((choices '("path" "clipboard" "list" "drop all"))
         (choice (completing-read "Attach: " choices nil t)))
    (pcase choice
      ("path" (call-interactively #'zerostack-add-file))
      ("clipboard" (zerostack-add-clipboard))
      ("list" (zerostack-list-files))
      ("drop all" (zerostack-drop-all-files)))))

(defun zerostack-add-file (path)
  "Attach file at PATH to the current zerostack session."
  (interactive (list (read-file-name "Attach file: " nil nil t)))
  (zerostack--send-command 'file-add
                           :request (zerostack--next-request)
                           :path (expand-file-name path)))

(defun zerostack-yank ()
  "Yank text, or attach image/media clipboard contents."
  (interactive)
  (unless (zerostack-add-clipboard t)
    (call-interactively #'yank)))

(defun zerostack-add-clipboard (&optional quiet)
  "Attach clipboard contents to the current zerostack session.

If the clipboard contains a path or file URI, attach that file.  If it contains
image bytes, write them to a temporary image file first.  Otherwise, write text
clipboard contents to a temporary text file and attach that.

When QUIET is non-nil, return nil instead of falling back to text attachment or
raising an error when no file/media clipboard content is available."
  (interactive)
  (let ((path (or (zerostack--clipboard-path)
                  (zerostack--clipboard-image-file)
                  (unless quiet
                    (zerostack--clipboard-text-file)))))
    (unless path
      (unless quiet
        (zerostack--set-notice "clipboard: no file path, image, or text found")
        (user-error "Clipboard does not contain a file path, image, or text")))
    (when path
      (zerostack--set-status (format "attaching clipboard: %s"
                                     (file-name-nondirectory path)))
      (zerostack-add-file path))
    path))

(defun zerostack--try-yank-media ()
  "Try Emacs' native `yank-media' clipboard path.

Return non-nil when a registered media handler attached something."
  (when (fboundp 'yank-media)
    (let ((before zerostack--request-counter))
      (condition-case nil
          (progn
            (yank-media)
            (> zerostack--request-counter before))
        (error nil)))))

(defun zerostack--yank-media-image (type data)
  "Attach pasted image DATA of MIME TYPE using `yank-media'."
  (let* ((mime (if (symbolp type) (symbol-name type) (format "%s" type)))
         (extension (zerostack--image-extension mime))
         (path (zerostack--write-clipboard-temp-file
                (string-as-unibyte data)
                extension
                t)))
    (zerostack--set-status (format "attaching pasted image: %s" mime))
    (zerostack-add-file path)))

(defun zerostack--image-extension (mime)
  "Return a file extension for image MIME."
  (pcase (downcase (or mime ""))
    ("image/jpeg" "jpg")
    ("image/jpg" "jpg")
    ("image/png" "png")
    ("image/gif" "gif")
    ("image/webp" "webp")
    (_
     (or (car (split-string (or (cadr (split-string (or mime "") "/")) "")
                            "[;+]" t))
         "img"))))

(defun zerostack-list-files ()
  "List files/media queued in the current zerostack session."
  (interactive)
  (zerostack--send-command 'file-list :request (zerostack--next-request)))

(defun zerostack-drop-all-files ()
  "Drop all files/media queued in the current zerostack session."
  (interactive)
  (when (yes-or-no-p "Drop all attached files/media? ")
    (zerostack--send-command 'file-drop-all :request (zerostack--next-request))))

(defun zerostack--clipboard-path ()
  "Return an existing file path described by the clipboard, if any."
  (or (zerostack--clipboard-path-from-selection)
      (zerostack--clipboard-path-from-command)
      (when-let ((text (zerostack--clipboard-text)))
        (zerostack--path-from-clipboard-text text))))

(defun zerostack--clipboard-path-from-selection ()
  "Return a file path from GUI clipboard file targets, if any."
  (catch 'path
    (dolist (target (append (zerostack--clipboard-file-targets)
                            (zerostack--clipboard-text-targets)))
      (when-let* ((text (zerostack--clipboard-selection target))
                  ((stringp text))
                  (path (zerostack--path-from-clipboard-text text)))
        (throw 'path path)))))

(defun zerostack--clipboard-path-from-command ()
  "Return a file path from platform clipboard command file targets, if any."
  (catch 'path
    (dolist (command '(("wl-paste" "--type" "text/uri-list" "--no-newline")
                       ("wl-paste" "--type" "x-special/gnome-copied-files" "--no-newline")
                       ("xclip" "-selection" "clipboard" "-t" "text/uri-list" "-o")
                       ("xclip" "-selection" "clipboard" "-t" "x-special/gnome-copied-files" "-o")))
      (when-let* ((text (apply #'zerostack--clipboard-command-output nil command))
                  (path (zerostack--path-from-clipboard-text text)))
        (throw 'path path)))))

(defun zerostack--clipboard-text ()
  "Return text currently available from the clipboard or kill ring."
  (or (catch 'text
        (dolist (target (zerostack--clipboard-text-targets))
          (when-let ((text (zerostack--clipboard-selection target)))
            (when (and (stringp text) (not (string-empty-p text)))
              (throw 'text text)))))
      (zerostack--clipboard-text-from-command)
      (ignore-errors (current-kill 0 t))))

(defun zerostack--clipboard-text-from-command ()
  "Return text from common platform clipboard commands."
  (catch 'text
    (dolist (command '(("wl-paste" "--no-newline")
                       ("xclip" "-selection" "clipboard" "-o")
                       ("pbpaste")))
      (when-let ((text (apply #'zerostack--clipboard-command-output nil command)))
        (unless (string-empty-p text)
          (throw 'text text))))))

(defun zerostack--clipboard-selection (target)
  "Return CLIPBOARD selection TARGET, ignoring unsupported targets."
  (when (fboundp 'gui-get-selection)
    (ignore-errors
      (gui-get-selection 'CLIPBOARD
                         (if (symbolp target) target (intern target))))))

(defun zerostack--clipboard-file-targets ()
  "Return GUI clipboard target symbols commonly used for copied files."
  (mapcar #'intern
          '("text/uri-list"
            "x-special/gnome-copied-files"
            "public.file-url")))

(defun zerostack--clipboard-text-targets ()
  "Return GUI clipboard target symbols commonly used for text."
  (append (mapcar #'intern '("text/plain;charset=utf-8" "text/plain"))
          '(UTF8_STRING STRING TEXT COMPOUND_TEXT)))

(defun zerostack--path-from-clipboard-text (text)
  "Return an existing path encoded in clipboard TEXT."
  (let ((candidates nil))
    (dolist (line (split-string text "\n" t))
      (let ((line (string-trim line)))
        (unless (or (string-empty-p line)
                    (string-prefix-p "#" line)
                    (member line '("copy" "cut" "move")))
          (push (zerostack--decode-clipboard-path line) candidates))))
    (cl-find-if #'file-exists-p (nreverse candidates))))

(defun zerostack--decode-clipboard-path (value)
  "Decode VALUE as a plain path or file URI."
  (let ((value (string-trim value "[ \t\n\r'\"]+" "[ \t\n\r'\"]+")))
    (when (string-prefix-p "file://" value)
      (setq value (substring value 7))
      (when (string-prefix-p "localhost/" value)
        (setq value (substring value (length "localhost"))))
      (when (and (fboundp 'url-unhex-string) (string-match-p "%" value))
        (setq value (url-unhex-string value))))
    (substitute-in-file-name value)))

(defun zerostack--clipboard-image-file ()
  "Write clipboard image data to a temporary file and return its path, if any."
  (or (zerostack--clipboard-image-from-selection)
      (zerostack--clipboard-image-from-command)))

(defun zerostack--clipboard-image-from-selection ()
  "Read image data directly from the GUI clipboard selection."
  (catch 'path
    (dolist (spec '(("image/png" . "png")
                    ("image/jpeg" . "jpg")
                    ("image/gif" . "gif")
                    ("image/webp" . "webp")))
      (let ((data (zerostack--clipboard-selection (car spec))))
        (when (and (stringp data) (> (length data) 0))
          (throw 'path
                 (zerostack--write-clipboard-temp-file
                  (string-as-unibyte data)
                  (cdr spec)
                  t)))))))

(defun zerostack--clipboard-image-from-command ()
  "Read image data from common platform clipboard commands."
  (catch 'path
    (dolist (cmd '(("wl-paste" "png" "--type" "image/png" "--no-newline")
                   ("wl-paste" "jpg" "--type" "image/jpeg" "--no-newline")
                   ("wl-paste" "gif" "--type" "image/gif" "--no-newline")
                   ("wl-paste" "webp" "--type" "image/webp" "--no-newline")
                   ("xclip" "png" "-selection" "clipboard" "-t" "image/png" "-o")
                   ("xclip" "jpg" "-selection" "clipboard" "-t" "image/jpeg" "-o")
                   ("xclip" "gif" "-selection" "clipboard" "-t" "image/gif" "-o")
                   ("xclip" "webp" "-selection" "clipboard" "-t" "image/webp" "-o")))
      (let ((program (car cmd))
            (ext (cadr cmd))
            (args (cddr cmd)))
        (when-let ((data (apply #'zerostack--clipboard-command-output t program args)))
          (throw 'path
                 (zerostack--write-clipboard-temp-file data ext t)))))
    (when (executable-find "pngpaste")
      (let ((path (make-temp-file "zerostack-clipboard-" nil ".png")))
        (when (eq (process-file "pngpaste" nil nil nil path) 0)
          (push path zerostack--clipboard-temp-files)
          path)))))

(defun zerostack--clipboard-command-output (binary program &rest args)
  "Return PROGRAM ARGS clipboard output or nil.

When BINARY is non-nil, preserve unibyte data."
  (when (executable-find program)
    (with-temp-buffer
      (when binary
        (set-buffer-multibyte nil))
      (let ((coding-system-for-read (if binary 'binary 'utf-8-unix))
            (coding-system-for-write (if binary 'binary 'utf-8-unix)))
        (when (eq (apply #'process-file program nil t nil args) 0)
          (let ((text (buffer-string)))
            (unless (string-empty-p text)
              text)))))))

(defun zerostack--clipboard-text-file ()
  "Write text clipboard content to a temporary file and return its path."
  (when-let ((text (zerostack--clipboard-text)))
    (unless (string-empty-p text)
      (zerostack--write-clipboard-temp-file text "txt" nil))))

(defun zerostack--write-clipboard-temp-file (data extension binary)
  "Write clipboard DATA to a temp file with EXTENSION.

When BINARY is non-nil, DATA is written with binary coding."
  (let ((path (make-temp-file "zerostack-clipboard-" nil (concat "." extension))))
    (with-temp-buffer
      (when binary
        (set-buffer-multibyte nil))
      (insert data)
      (let ((coding-system-for-write (if binary 'binary 'utf-8-unix)))
        (write-region (point-min) (point-max) path nil 'silent)))
    (push path zerostack--clipboard-temp-files)
    path))

(defun zerostack--cleanup-clipboard-temp-files ()
  "Remove temporary files created from clipboard attachments."
  (dolist (path zerostack--clipboard-temp-files)
    (ignore-errors
      (when (file-exists-p path)
        (delete-file path))))
  (setq zerostack--clipboard-temp-files nil))

(defun zerostack-permission-answer (request decision &optional pattern)
  "Answer permission REQUEST with DECISION and optional allow-always PATTERN."
  (interactive
   (list (read-number "Permission request: ")
         (intern (completing-read "Decision: " '("allow-once" "allow-always" "deny") nil t))
         nil))
  ;; The protocol currently uses :request for the permission id itself.
  (if (and pattern (not (string-empty-p (string-trim pattern))))
      (zerostack--send-command 'permission-answer
                               :request request
                               :decision decision
                               :pattern pattern)
    (zerostack--send-command 'permission-answer
                             :request request
                             :decision decision)))

(defun zerostack-insert-newline ()
  "Insert a newline into the current prompt input."
  (interactive)
  (zerostack--ensure-prompt)
  (let ((inhibit-read-only t))
    (goto-char (max (marker-position zerostack--input-marker)
                    (min (point) (marker-position zerostack--controls-start-marker))))
    (insert "\n")))

(defun zerostack-send-input ()
  "Send the current input line as a normal zerostack prompt."
  (interactive)
  (zerostack--ensure-prompt)
  (let ((text (string-trim-right
               (buffer-substring-no-properties
                (marker-position zerostack--input-marker)
                (marker-position zerostack--controls-start-marker)))))
    (zerostack--clear-input)
    (cond
     ((string-empty-p text)
      (zerostack--append-local-line "empty input" 'zs-muted))
     (t
      (zerostack-send-prompt text)))))

(when (featurep 'hydra)
  (defhydra zerostack-command-hydra (:hint nil :color blue)
    "
Zerostack
_k_ skill  _a_ attach  _c_ compact  _w_ rewind  _u_ redo  _g_ goal  _G_ clear goal  _l_ loop  _t_ thinking  _p_ provider  _m_ model  _P_ subagent provider  _S_ subagent model  _T_ tools  _M_ MCP  _v_ view  _o_ artifact  _R_ restart
"
    ("k" zerostack-skill-menu)
    ("a" zerostack-attachment-menu)
    ("c" zerostack-compact)
    ("w" zerostack-rewind)
    ("u" zerostack-redo)
    ("g" zerostack-goal-set)
    ("G" zerostack-clear-goal)
    ("l" zerostack-loop)
    ("t" zerostack-thinking-menu)
    ("p" zerostack-provider-menu)
    ("m" zerostack-model-menu)
    ("P" zerostack-subagent-provider-menu)
    ("S" zerostack-subagent-model-menu)
    ("T" zerostack-list-tools)
    ("M" zerostack-mcp)
    ("v" zerostack-set-view)
    ("o" zerostack-open-last-artifact)
    ("R" zerostack-restart-daemon)))

(defun zerostack-command-menu ()
  "Open the zerostack command menu."
  (interactive)
  (if (fboundp 'zerostack-command-hydra/body)
      (zerostack-command-hydra/body)
    (zerostack--command-menu-fallback)))

(defun zerostack--command-menu-fallback ()
  "Fallback command menu used when Hydra is unavailable."
  (let* ((commands '("skill" "attach" "compact" "rewind" "redo" "loop" "thinking"
                    "provider" "model" "subagent-provider" "subagent-model" "goal"
                    "clear-goal" "tools" "mcp" "view" "artifact" "restart"))
         (choice (completing-read "Zerostack command: " commands nil t)))
    (pcase choice
      ("skill" (zerostack-skill-menu))
      ("attach" (zerostack-attachment-menu))
      ("compact" (call-interactively #'zerostack-compact))
      ("rewind" (call-interactively #'zerostack-rewind))
      ("redo" (call-interactively #'zerostack-redo))
      ("loop" (call-interactively #'zerostack-loop))
      ("thinking" (call-interactively #'zerostack-thinking-menu))
      ("provider" (call-interactively #'zerostack-provider-menu))
      ("model" (call-interactively #'zerostack-model-menu))
      ("subagent-provider" (call-interactively #'zerostack-subagent-provider-menu))
      ("subagent-model" (call-interactively #'zerostack-subagent-model-menu))
      ("goal" (call-interactively #'zerostack-goal))
      ("clear-goal" (call-interactively #'zerostack-clear-goal))
      ("tools" (call-interactively #'zerostack-list-tools))
      ("mcp" (call-interactively #'zerostack-mcp))
      ("view" (call-interactively #'zerostack-set-view))
      ("artifact" (zerostack-open-last-artifact))
      ("restart" (zerostack-restart-daemon)))))

(defun zerostack-permission-menu ()
  "Select and answer a pending permission request."
  (interactive)
  (let ((requests nil))
    (maphash
     (lambda (request plist)
       (push (cons (format "%s  %s" request (or (plist-get plist :tool) "tool"))
                   request)
             requests))
     zerostack--pending-permissions)
    (if (null requests)
        (zerostack--set-notice "no pending permissions")
      (let* ((label (completing-read "Permission: " (mapcar #'car requests) nil t))
             (request (cdr (assoc label requests)))
             (decision (completing-read "Decision: " '("allow-once" "allow-always" "deny") nil t))
             (pattern (when (equal decision "allow-always")
                        (read-string "Allow-always pattern: "))))
        (zerostack-permission-answer request (intern decision) pattern)))))

(defun zerostack-skill-menu ()
  "Select a discovered skill and insert a skill directive at the prompt."
  (interactive)
  (let ((skills (zerostack--discover-skills)))
    (when (null skills)
      (zerostack--update-session-metadata-from-board)
      (setq skills (zerostack--discover-skills)))
    (if (null skills)
        (zerostack--set-notice "no skills discovered")
      (let* ((labels (mapcar (lambda (skill)
                               (cons (format "%s — %s"
                                             (plist-get skill :name)
                                             (plist-get skill :description))
                                     skill))
                             skills))
             (label (completing-read "Skill: " (mapcar #'car labels) nil t))
             (skill (cdr (assoc label labels))))
        (zerostack--insert-input
         (format "Use the %s skill at %s. "
                 (plist-get skill :name)
                 (plist-get skill :path)))
        (zerostack--set-notice (format "selected skill: %s" (plist-get skill :name)))))))

(defun zerostack--update-session-metadata-from-board ()
  "Refresh chat metadata from a board snapshot when status has not arrived yet."
  (when (or zerostack--session zerostack--socket)
    (when-let ((match (ignore-errors
                        (zerostack--find-session-in-board (zerostack-board--fetch)))))
      (let ((session (plist-get match :session))
            (worktree (plist-get match :worktree)))
        (zerostack--set-session-metadata (plist-get session :title)
                                         (plist-get session :cwd)
                                         (plist-get worktree :path))
        t))))

(defun zerostack--find-session-in-board (snapshot)
  "Return board match plist for this chat session in SNAPSHOT."
  (let ((projects (plist-get (cdr snapshot) :projects))
        found)
    (dolist (project projects found)
      (dolist (worktree (plist-get project :worktrees))
        (dolist (session (plist-get worktree :sessions))
          (when (and (not found)
                     (zerostack--board-session-matches-chat-p session))
            (setq found (list :project project
                              :worktree worktree
                              :session session))))))))

(defun zerostack--board-session-matches-chat-p (session)
  "Return non-nil when board SESSION describes the current chat buffer."
  (or (and zerostack--session
           (equal (plist-get session :id) zerostack--session))
      (and zerostack--socket
           (equal (plist-get session :socket) zerostack--socket))))

(defun zerostack--dispatch-slash (text)
  "Dispatch slash command TEXT in the current zerostack buffer."
  (let* ((parts (split-string text "[ \t]+" t))
         (cmd (car parts))
         (args (cdr parts))
         (rest (string-trim (or (string-remove-prefix cmd text) ""))))
    (pcase cmd
      ((or "/help" "/?") (zerostack--show-help))
      ((or "/quit" "/exit") (zerostack-disconnect))
      ("/attach" (zerostack-attach))
      ("/render" (zerostack-render))
      ("/status" (zerostack-request-status))
      ("/sessions" (zerostack-request-sessions (and (car args) (string-to-number (car args)))))
      ("/view"
       (if (car args)
           (zerostack-set-view (string-to-number (car args)))
         (zerostack--append-local-line "usage: /view <cols>" 'zs-error)))
      ("/abort" (zerostack-abort))
      ((or "/compact" "/compress") (zerostack-compact rest))
      ("/rewind"
       (if (car args)
           (zerostack-rewind (string-to-number (car args)))
         (zerostack-rewind)))
      ("/redo" (zerostack-redo))
      ("/permission" (zerostack--slash-permission args))
      ("/allow" (zerostack--slash-permission
                 (append (list (car args) "allow-once") (cdr args))))
      ("/allow-always" (zerostack--slash-permission
                        (append (list (car args) "allow-always") (cdr args))))
      ("/deny" (zerostack--slash-permission
                (append (list (car args) "deny") (cdr args))))
      ("/artifact" (zerostack-open-last-artifact))
      ("/latex" (zerostack-open-last-latex-artifact))
      ("/goal" (zerostack-goal nil))
      ((or "/tools" "/list-tools") (zerostack-list-tools))
      ("/mcp" (zerostack-mcp))
      ((or "/thinking" "/reasoning")
       (if (car args)
           (zerostack-thinking-menu (car args))
         (call-interactively #'zerostack-thinking-menu)))
      ("/loop"
       (zerostack--append-local-line
        "/loop is not implemented by the native Emacs protocol yet"
        'zs-error))
      (_
       (zerostack--append-local-line
        (format "unknown zerostack command: %s" cmd)
        'zs-error)))))

(defun zerostack--slash-permission (args)
  "Handle permission slash ARGS."
  (let* ((request (car args))
         (decision (cadr args))
         (pattern (string-join (cddr args) " ")))
    (cond
     ((or (null request) (string-empty-p request))
      (zerostack--append-local-line
       "usage: /permission <request> <allow-once|allow-always|deny> [pattern]"
       'zs-error))
     ((or (null decision) (string-empty-p decision))
      (zerostack--append-local-line
       "usage: /permission <request> <allow-once|allow-always|deny> [pattern]"
       'zs-error))
     (t
      (zerostack-permission-answer
       (string-to-number request)
       (intern decision)
       (unless (string-empty-p pattern) pattern))))))

(defun zerostack--show-help ()
  "Show concise command-menu help in the prompt status line."
   (zerostack--set-notice
    "commands: C-c / opens skill, attach, compact, rewind, redo, loop, thinking, provider, model, mcp, view, artifact, restart"))

(defun zerostack-restart-daemon ()
  "Restart this buffer's zerostack daemon without closing the buffer."
  (interactive)
  (let ((args (or zerostack--server-args
                  (and zerostack--session (list "--session" zerostack--session)))))
    (zerostack--delete-current-processes)
    (setq zerostack--socket nil
          zerostack--line-buffer "")
    (zerostack--append-local-line "restarting zerostack --emacs" 'zs-muted)
    (zerostack--start-server args)))

(defun zerostack-disconnect ()
  "Disconnect from zerostack and stop a server process started by this buffer."
  (interactive)
  (zerostack--cleanup-clipboard-temp-files)
  (zerostack--delete-current-processes)
  (zerostack--append-local-line "disconnected" 'zs-muted))

(defun zerostack--handle-form (form)
  "Handle one decoded protocol FORM."
  (pcase (car-safe form)
    ('ready (zerostack--handle-ready (cdr form)))
    ('ok (zerostack--handle-ok (cdr form)))
    ('error (zerostack--handle-error (cdr form)))
    ('sessions (zerostack--handle-sessions (cdr form)))
    ('status (zerostack--handle-status (cdr form)))
    ('event (zerostack--handle-event (cdr form)))
    (_ (zerostack--append-local-line (format "unknown form: %S" form) 'zs-error))))

(defun zerostack--handle-ready (plist)
  "Handle ready PLIST."
  (setq zerostack--protocol (plist-get plist :protocol)
        zerostack--session (plist-get plist :session)
        zerostack--pid (plist-get plist :pid)
        zerostack--socket (zerostack--normalize-socket (plist-get plist :socket)))
  (unless (zerostack--dedupe-current-chat-buffer)
    (zerostack--rename-chat-buffer)))

(defun zerostack--handle-ok (plist)
  "Handle ok PLIST."
  (when-let ((cols (plist-get plist :cols)))
    (setq zerostack--cols cols))
  (zerostack--update-provider-model plist)
  (when (plist-member plist :active)
    (zerostack--update-loop-state plist))
  (when-let ((message (plist-get plist :message)))
    (zerostack--append-local-line message 'zs-muted))
  (when-let ((text (plist-get plist :text)))
    (zerostack--insert-input (format "%s" text)))
  (when (plist-member plist :compacted)
    (when (plist-get plist :compacted)
      (zerostack-render))
    (zerostack--set-thinking nil)
    (zerostack--set-status nil)))

(defun zerostack--update-provider-model (plist)
  "Update provider/model buffer-local state from PLIST."
  (when-let ((provider (plist-get plist :provider)))
    (setq zerostack--provider (format "%s" provider)))
  (when-let ((model (plist-get plist :model)))
    (setq zerostack--model (format "%s" model)))
  (when-let ((provider (plist-get plist :subagent-provider)))
    (setq zerostack--subagent-provider (format "%s" provider)))
  (when-let ((model (plist-get plist :subagent-model)))
    (setq zerostack--subagent-model (format "%s" model)))
  (when-let ((tokens (plist-get plist :tokens)))
    (setq zerostack--tokens tokens))
  (when-let ((tokens (plist-get plist :reasoning-tokens)))
    (setq zerostack--reasoning-tokens tokens))
  (when-let ((window (plist-get plist :context-window)))
    (setq zerostack--context-window window))
  (when-let ((level (plist-get plist :thinking)))
    (setq zerostack--thinking-level (format "%s" level)))
  (when (plist-member plist :reasoning-effort-supported)
    (setq zerostack--reasoning-effort-supported
          (not (null (plist-get plist :reasoning-effort-supported)))))
  (when (plist-member plist :reasoning-effort)
    (setq zerostack--reasoning-effort
          (when-let ((effort (plist-get plist :reasoning-effort)))
            (format "%s" effort))))
  (when (plist-member plist :reasoning-efforts)
    (setq zerostack--reasoning-efforts
          (cl-remove-if-not #'stringp (plist-get plist :reasoning-efforts))))
  (when (and (markerp zerostack--prompt-start-marker)
             (markerp zerostack--input-marker)
             (marker-position zerostack--prompt-start-marker)
             (marker-position zerostack--input-marker))
    (zerostack--refresh-prompt)))

(defun zerostack--handle-error (plist)
  "Handle error PLIST."
  (zerostack--clear-pending-permissions)
  (setq zerostack--loop-active nil
        zerostack--loop-label nil)
  (zerostack--set-thinking nil)
  (zerostack--set-status nil)
  (zerostack--append-local-line
   (format "error: %s" (or (plist-get plist :message) plist))
   'zs-error))

(defun zerostack--handle-sessions (plist)
  "Handle sessions response PLIST."
  (let ((items (plist-get plist :items)))
    (if items
        (zerostack--set-notice (format "sessions: %d live" (length items)))
      (zerostack--set-notice "sessions: none"))))

(defun zerostack--handle-status (plist)
  "Handle status response PLIST."
  (let* ((request (plist-get plist :request))
         (session (plist-get plist :session))
         (internal (and zerostack--metadata-status-request
                        (equal request zerostack--metadata-status-request))))
    (zerostack--set-session-metadata (plist-get session :title)
                                     (plist-get session :cwd)
                                     (plist-get session :cwd))
    (zerostack--update-provider-model session)
    (when internal
      (setq zerostack--metadata-status-request nil))
    (unless internal
      (zerostack--set-notice
       (format "status: %s pid:%s"
               (plist-get session :session)
               (plist-get session :pid))))))

(defun zerostack--handle-event (plist)
  "Handle event PLIST."
  (pcase (plist-get plist :type)
    ('session-render
     (zerostack--clear-render-caches)
     (zerostack--replace-lines (or (plist-get plist :replace-from) 0)
                               (or (plist-get plist :lines) nil)))
    ('session-prepend
     (zerostack--queue-prepend-lines (or (plist-get plist :lines) nil)))
    ((or 'user-render 'assistant-render 'reasoning-render 'tool-render 'error-render 'retry-render)
     (when (memq (plist-get plist :type)
                 '(assistant-render reasoning-render tool-render retry-render))
       (zerostack--set-thinking t)
       (unless zerostack--status
         (zerostack--set-status "thinking...")))
     (zerostack--replace-lines (or (plist-get plist :replace-from) 0)
                               (or (plist-get plist :lines) nil)))
    ('loop-started
     (zerostack--update-loop-state plist)
     (zerostack--set-thinking t))
    ('loop-iteration
     (zerostack--update-loop-state plist)
     (zerostack--set-thinking t))
    ('loop-stopped
     (setq zerostack--loop-active nil
           zerostack--loop-label nil)
     (zerostack--clear-pending-permissions)
     (zerostack--set-thinking nil)
     (zerostack--set-status nil)
     (zerostack--set-notice
      (if (eq (plist-get plist :reason) 'max)
          "loop stopped: max iterations reached"
        "loop stopped")))
    ('goal-nudge
     (zerostack--clear-pending-permissions)
     (zerostack--set-thinking t)
     (zerostack--set-status "continuing goal")
     (when-let ((message (plist-get plist :message)))
       (zerostack--set-notice message))
     (zerostack-board--refresh-if-visible))
    ('compact-started
     (zerostack--set-status "compacting...")
     (zerostack--set-thinking t))
    ((or 'compact-done 'compact-finished)
     (if (eq (plist-get plist :mid-turn) t)
         (progn
           (zerostack--set-thinking t)
           (zerostack--set-status "continuing..."))
       (when (plist-get plist :compacted)
         (zerostack-render))
       (zerostack--set-thinking nil)
       (zerostack--set-status nil))
     (zerostack-board--refresh-if-visible))
    ('tool-call
     (zerostack--set-thinking t))
    ('subagent-tool-call
     (zerostack--set-thinking t))
    ('tool-result
     (zerostack--set-thinking t)
     (zerostack--remember-artifact (plist-get plist :artifact)))
    ('reasoning
     (zerostack--set-thinking t)
     (zerostack--remember-artifact (plist-get plist :artifact)))
    ('permission-request
     (zerostack--handle-permission-request plist)
     (zerostack-board--refresh-if-visible))
    ('permission-answered
     (when-let ((request (plist-get plist :request)))
       (remhash request zerostack--pending-permissions))
     (zerostack--refresh-permission-status))
    ('completion-call
     (zerostack--set-thinking t)
     (zerostack--update-provider-model plist))
    ('retry
     (zerostack--set-thinking t))
    ('done
     (zerostack--update-provider-model plist)
     (zerostack--clear-pending-permissions)
     (zerostack--set-thinking nil)
     (unless zerostack--loop-active
       (zerostack--set-status nil))
     (zerostack-board--refresh-if-visible))
    ('latex-preview-ready
     (zerostack--handle-latex-preview-ready (plist-get plist :items)))
    ('aborted
     (zerostack--clear-pending-permissions)
     (setq zerostack--loop-active nil
           zerostack--loop-label nil)
     (zerostack--set-thinking nil)
     (zerostack--set-status nil)
     (zerostack--set-notice "aborted"))
    ('error
     (zerostack--clear-pending-permissions)
     (setq zerostack--loop-active nil
           zerostack--loop-label nil)
     (zerostack--set-thinking nil)
     (when-let ((message (plist-get plist :message)))
       (zerostack--set-notice message)))
    (_
     (zerostack--append-local-line
      (format "event: %S" (plist-get plist :type))
      'zs-muted))))

(defun zerostack--update-loop-state (plist)
  "Update local loop state from PLIST."
  (setq zerostack--loop-active (eq (plist-get plist :active) t)
        zerostack--loop-label (plist-get plist :label))
  (if zerostack--loop-active
      (zerostack--set-status
       (format "loop %s" (or zerostack--loop-label
                             (plist-get plist :iteration)
                             "active")))
    (setq zerostack--loop-label nil)))

(defun zerostack--handle-permission-request (plist)
  "Handle permission request event PLIST."
  (let ((request (plist-get plist :request)))
    (puthash request plist zerostack--pending-permissions)
    (zerostack--refresh-permission-status)
    (zerostack--notify-needs-input
     (format "permission #%s %s"
             request
             (or (plist-get plist :tool) "tool")))))

(defun zerostack--needs-input-p ()
  "Return non-nil when the current session is waiting for user input."
  (zerostack--pending-permissions-p))

(defun zerostack--pending-permissions-p ()
  "Return non-nil when the current session is waiting for permission."
  (and (hash-table-p zerostack--pending-permissions)
       (> (hash-table-count zerostack--pending-permissions) 0)))

(defun zerostack--first-pending-permission ()
  "Return one pending permission plist, preferring the lowest request id."
  (let (best-key best-value)
    (when (hash-table-p zerostack--pending-permissions)
      (maphash
       (lambda (key value)
         (when (or (null best-key) (< key best-key))
           (setq best-key key
                 best-value value)))
       zerostack--pending-permissions))
    best-value))

(defun zerostack--pending-permission-list ()
  "Return pending permission plists sorted by request id."
  (let (items)
    (when (hash-table-p zerostack--pending-permissions)
      (maphash (lambda (_request plist) (push plist items))
               zerostack--pending-permissions))
    (sort items (lambda (left right)
                  (< (or (plist-get left :request) 0)
                     (or (plist-get right :request) 0))))))

(defun zerostack--clear-pending-permissions ()
  "Clear pending permission state and refresh the prompt line."
  (when (hash-table-p zerostack--pending-permissions)
    (clrhash zerostack--pending-permissions))
  (when (and (markerp zerostack--prompt-start-marker)
             (marker-position zerostack--prompt-start-marker))
    (zerostack--refresh-prompt)
    (zerostack--refresh-permission-buttons)))

(defun zerostack--refresh-permission-status ()
  "Refresh the single-line prompt status for pending permissions."
  (if-let ((permission (zerostack--first-pending-permission)))
      (zerostack--set-status
       (format "permission #%s %s"
               (plist-get permission :request)
               (or (plist-get permission :tool) "tool")))
    (zerostack--set-status nil))
  (zerostack--refresh-permission-buttons))

(defun zerostack--refresh-permission-buttons ()
  "Refresh clickable pending-permission buttons below the input prompt."
  (zerostack--ensure-prompt)
  (zerostack--without-undo
    (let ((saved-point (copy-marker (point) nil))
          (input-end (marker-position zerostack--controls-start-marker))
          (inhibit-read-only t))
      (unwind-protect
          (progn
            (set-marker-insertion-type zerostack--controls-start-marker nil)
            (delete-region input-end (point-max))
            (goto-char input-end)
            (when (zerostack--pending-permissions-p)
              (insert (propertize "\npermission: "
                                  'face 'zerostack-muted-face
                                  'read-only t
                                  'rear-nonsticky t))
              (let ((first t))
                (dolist (permission (zerostack--pending-permission-list))
                  (unless first
                    (insert (propertize "  " 'read-only t 'rear-nonsticky t)))
                  (setq first nil)
                  (zerostack--insert-permission-buttons permission))))
            (set-marker zerostack--controls-start-marker input-end)
            (set-marker-insertion-type zerostack--controls-start-marker t)
            (goto-char (min (marker-position zerostack--controls-start-marker)
                            (marker-position saved-point))))
        (set-marker saved-point nil)))))

(defun zerostack--insert-permission-buttons (permission)
  "Insert buttons for one pending PERMISSION request."
  (let* ((request (plist-get permission :request))
         (tool (or (plist-get permission :tool) "tool"))
         (input (or (plist-get permission :input) "")))
    (insert (propertize (format "#%s %s " request tool)
                        'face 'zerostack-muted-face
                        'read-only t
                        'rear-nonsticky t))
    (zerostack--insert-permission-button "allow once" permission 'allow-once)
    (insert (propertize " " 'read-only t 'rear-nonsticky t))
    (zerostack--insert-permission-button "allow always" permission 'allow-always)
    (insert (propertize " " 'read-only t 'rear-nonsticky t))
    (zerostack--insert-permission-button "deny" permission 'deny)
    (unless (string-empty-p input)
      (insert (propertize (format "  %s" (zerostack--status-text input))
                          'face 'zerostack-muted-face
                          'read-only t
                          'rear-nonsticky t)))))

(defun zerostack--insert-permission-button (label permission decision)
  "Insert a clickable permission button LABEL for PERMISSION/DECISION."
  (insert-text-button
   label
   'face 'zerostack-link-face
   'read-only t
   'rear-nonsticky t
   'mouse-face 'highlight
   'follow-link t
   'help-echo (format "%s permission #%s"
                      (capitalize (replace-regexp-in-string "-" " " (symbol-name decision)))
                      (plist-get permission :request))
   'zerostack-permission permission
   'zerostack-decision decision
   'action (lambda (button)
             (zerostack--permission-button-action button))))

(defun zerostack--permission-button-action (button)
  "Answer the pending permission represented by BUTTON."
  (let* ((permission (button-get button 'zerostack-permission))
         (decision (button-get button 'zerostack-decision))
         (request (plist-get permission :request))
         (pattern (when (eq decision 'allow-always)
                    (read-string "Allow-always pattern: "
                                 (or (plist-get permission :suggested-pattern) "")))))
    (zerostack-permission-answer request decision pattern)))

(defun zerostack--queue-prepend-lines (lines)
  "Queue older rendered logical LINES for idle backfill."
  (when lines
    (setq zerostack--backfill-queue (append zerostack--backfill-queue (list lines)))
    (unless (timerp zerostack--backfill-timer)
      (setq zerostack--backfill-timer
            (run-at-time 0.01 0.01 #'zerostack--backfill-step (current-buffer))))))

(defun zerostack--backfill-step (buffer)
  "Insert one queued history chunk into BUFFER."
  (if (not (buffer-live-p buffer))
      (when (timerp zerostack--backfill-timer)
        (cancel-timer zerostack--backfill-timer))
    (with-current-buffer buffer
      (if zerostack--backfill-queue
          (let ((lines (pop zerostack--backfill-queue)))
            (zerostack--prepend-lines lines))
        (when (timerp zerostack--backfill-timer)
          (cancel-timer zerostack--backfill-timer))
        (setq zerostack--backfill-timer nil)))))

(defun zerostack--prepend-lines (lines)
  "Insert rendered logical LINES before the current transcript."
  (zerostack--ensure-prompt)
  (when lines
    (zerostack--without-undo
      (let ((saved-point (copy-marker (point) nil)))
        (unwind-protect
            (let ((new-markers nil)
                  (inhibit-read-only t)
                  (start (if zerostack--line-markers
                             (marker-position (car zerostack--line-markers))
                           (marker-position zerostack--notice-start-marker))))
              (save-excursion
                (goto-char start)
                (dolist (line lines)
                  (let ((marker (copy-marker (point) nil)))
                    (push marker new-markers)
                    (zerostack--insert-wire-line line)))
                (setq zerostack--line-markers
                      (append (nreverse new-markers) zerostack--line-markers))))
          (goto-char saved-point)
          (set-marker saved-point nil))))))

(defun zerostack--replace-lines (replace-from lines)
  "Replace rendered logical lines from REPLACE-FROM with LINES."
  (zerostack--ensure-prompt)
  (zerostack--without-undo
    (let ((saved-point (copy-marker (point) nil)))
      (unwind-protect
          (let* ((keep (min (max 0 replace-from) (length zerostack--line-markers)))
                 (prefix (cl-subseq zerostack--line-markers 0 keep))
                 (new-markers nil)
                 (start (if (< keep (length zerostack--line-markers))
                            (marker-position (nth keep zerostack--line-markers))
                          (marker-position zerostack--notice-start-marker)))
                 (end (marker-position zerostack--notice-start-marker))
                 (old-tail (nthcdr keep zerostack--line-markers))
                 (inhibit-read-only t))
            (mapc (lambda (marker) (set-marker marker nil)) old-tail)
            (setq zerostack--line-markers prefix)
            (remove-overlays start end 'zerostack-latex t)
            (delete-region start end)
            (goto-char start)
            (dolist (line lines)
              (let ((marker (copy-marker (point) nil)))
                (push marker new-markers)
                (zerostack--insert-wire-line line)))
            (set-marker zerostack--notice-start-marker (point))
            (setq zerostack--line-markers (append prefix (nreverse new-markers)))
            (goto-char saved-point))
        (set-marker saved-point nil)))))

(defun zerostack--insert-wire-line (line)
  "Insert one pre-rendered LINE plist at point."
  (let* ((text (or (plist-get line :text) ""))
         (face (zerostack--face (or (plist-get line :face) 'zs-normal)))
         (spans (plist-get line :spans))
         (artifact (plist-get line :artifact))
         (latex (plist-get line :latex))
         (message-index (plist-get line :message-index))
         (role (plist-get line :role))
         (start (point)))
    (if spans
        (zerostack--insert-wire-spans spans face)
      (insert text)
      (add-text-properties start (point) `(face ,face read-only t rear-nonsticky t)))
    (when message-index
      (add-text-properties
       start (point)
       `(zerostack-message-index ,message-index zerostack-message-role ,role)))
    (when artifact
      (zerostack--remember-artifact artifact)
      (zerostack--make-artifact-region start (point) artifact))
    (when latex
      (dolist (item latex)
        (zerostack--remember-latex item)))
    (insert (propertize "\n" 'read-only t 'rear-nonsticky t))))

(defun zerostack--insert-wire-spans (spans fallback-face)
  "Insert line-local SPANS with protocol faces."
  (if spans
      (dolist (span spans)
        (let ((start (point))
              (text (or (plist-get span :text) ""))
              (face (zerostack--face (or (plist-get span :face) 'zs-normal))))
          (insert text)
          (add-text-properties
           start (point)
           `(face ,face read-only t rear-nonsticky t))))
    (let ((start (point)))
      (add-text-properties
       start (point)
       `(face ,fallback-face read-only t rear-nonsticky t)))))

(defun zerostack--make-artifact-region (start end artifact)
  "Make text between START and END open ARTIFACT."
  (when (< start end)
    (add-text-properties
     start end
     `(face ,(zerostack--artifact-region-face start)
	    mouse-face highlight
	    help-echo ,(format "Open artifact: %s" (plist-get artifact :path))
	    keymap ,zerostack-artifact-map
	    follow-link t
	    zerostack-artifact ,artifact))))

(defun zerostack--artifact-region-face (pos)
  "Return face for an artifact link at POS, preserving its base styling."
  (let ((face (get-text-property pos 'face)))
    (cond
     ((null face) 'zerostack-link-face)
     ((eq face 'zerostack-link-face) face)
     ((and (listp face) (memq 'zerostack-link-face face)) face)
     ((listp face) (cons 'zerostack-link-face face))
     (t (list 'zerostack-link-face face)))))

(defun zerostack--clear-render-caches ()
  "Clear metadata tied to the current rendered transcript."
  (setq zerostack--artifacts nil)
  (when (hash-table-p zerostack--latex-items)
    (clrhash zerostack--latex-items))
  (mapc #'delete-overlay zerostack--latex-overlays)
  (setq zerostack--latex-overlays nil))

(defun zerostack--remember-artifact (artifact)
  "Remember ARTIFACT if non-nil."
  (when artifact
    (let ((path (plist-get artifact :path)))
      (unless (cl-find path zerostack--artifacts
                       :key (lambda (it) (plist-get it :path))
                       :test #'equal)
        (setq zerostack--artifacts (append zerostack--artifacts (list artifact)))))))

(defun zerostack--remember-latex (item)
  "Remember LaTeX ITEM metadata."
  (when-let ((id (plist-get item :id)))
    (puthash id item zerostack--latex-items)
    (zerostack--remember-artifact (plist-get item :artifact))
    (zerostack--remember-artifact (plist-get item :svg-artifact))))

(defun zerostack--handle-latex-preview-ready (items)
  "Install inline overlays for LaTeX ITEMS."
  (dolist (item items)
    (zerostack--remember-latex item)
    (zerostack--install-latex-overlay item)))

(defun zerostack--install-latex-overlay (item)
  "Create a clickable overlay for LaTeX ITEM."
  (let* ((start (zerostack--position-for-line-column
                 (plist-get item :line-start)
                 (plist-get item :col-start)))
         (end (zerostack--position-for-line-column
               (plist-get item :line-end)
               (plist-get item :col-end)))
         (artifact (plist-get item :artifact)))
    (when (and start end (< start end))
      (let ((overlay (make-overlay start end nil nil t)))
        (overlay-put overlay 'zerostack-latex t)
        (overlay-put overlay 'face 'zerostack-latex-face)
        (overlay-put overlay 'mouse-face 'highlight)
        (overlay-put overlay 'help-echo
                     (format "LaTeX %s: %s"
                             (plist-get item :id)
                             (plist-get artifact :path)))
        (overlay-put overlay 'keymap zerostack-artifact-map)
        (overlay-put overlay 'follow-link t)
        (overlay-put overlay 'zerostack-artifact artifact)
        (when-let ((display (or (zerostack--latex-svg-display item)
                                (and zerostack-auctex-preview
                                     zerostack-auctex-fold
                                     (zerostack--latex-inline-display item)))))
          (overlay-put overlay 'display display))
        (push overlay zerostack--latex-overlays)))))

(defun zerostack--latex-svg-display (item)
  "Return an inline SVG image descriptor for LaTeX ITEM, or nil."
  (when-let* ((artifact (plist-get item :svg-artifact))
              (path (plist-get artifact :path)))
    (when (and (stringp path)
               (file-readable-p path)
               (image-type-available-p 'svg))
      (ignore-errors
        (create-image path 'svg nil :ascent 'center)))))

(defun zerostack--position-for-line-column (line column)
  "Return buffer position for zero-based LINE and COLUMN, or nil."
  (when-let ((marker (nth line zerostack--line-markers)))
    (save-excursion
      (goto-char marker)
      (let ((line-end (line-end-position)))
        (min line-end (+ (point) (or column 0)))))))

(defun zerostack--latex-inline-display (item)
  "Return a folded inline display string for LaTeX ITEM, or nil."
  (when-let ((source (plist-get item :source)))
    (when (and (not (string-empty-p source))
               (require 'latex nil t)
               (require 'tex-fold nil t)
               (fboundp 'LaTeX-mode)
               (fboundp 'TeX-fold-mode)
               (fboundp 'TeX-fold-buffer)
               (fboundp 'TeX-fold-buffer-substring))
      (ignore-errors
        (with-temp-buffer
          (insert "\\documentclass{article}\n")
          (insert "\\usepackage{amsmath,amssymb}\n")
          (insert "\\begin{document}\n")
          (let ((start (point)))
            (insert source)
            (let ((end (point)))
              (insert "\n\\end{document}\n")
              (LaTeX-mode)
              (zerostack--enable-tex-fold)
              (let ((folded (string-trim
                             (replace-regexp-in-string
                              "[[:space:]]+" " "
                              (TeX-fold-buffer-substring start end)))))
                (unless (string-empty-p folded)
                  folded)))))))))

(defun zerostack--enable-tex-fold ()
  "Enable AUCTeX folding in the current temporary LaTeX buffer."
  (when (and (require 'tex-fold nil t)
             (fboundp 'TeX-fold-mode))
    (TeX-fold-mode 1)
    (when (fboundp 'TeX-fold-buffer)
      (ignore-errors
        (TeX-fold-buffer)))))

(defun zerostack-open-artifact-at-point (&optional event)
  "Open the artifact at point or mouse EVENT."
  (interactive (list last-nonmenu-event))
  (when (eventp event)
    (posn-set-point (event-end event)))
  (let ((artifact (or (get-text-property (point) 'zerostack-artifact)
                      (cl-some (lambda (overlay)
                                 (overlay-get overlay 'zerostack-artifact))
                               (overlays-at (point))))))
    (if artifact
        (zerostack--open-artifact artifact)
      (zerostack--append-local-line "no artifact at point" 'zs-error))))

(defun zerostack-open-last-artifact ()
  "Open the most recently advertised artifact."
  (interactive)
  (if-let ((artifact (car (last zerostack--artifacts))))
      (zerostack--open-artifact artifact)
    (zerostack--append-local-line "no artifacts yet" 'zs-error)))

(defun zerostack-open-last-latex-artifact ()
  "Open the most recently advertised LaTeX source artifact."
  (interactive)
  (let ((latex-artifacts
         (cl-remove-if-not
          (lambda (artifact)
            (eq (plist-get artifact :kind) 'latex-source))
          zerostack--artifacts)))
    (if-let ((artifact (car (last latex-artifacts))))
        (zerostack--open-artifact artifact)
      (zerostack--append-local-line "no LaTeX artifacts yet" 'zs-error))))

(defun zerostack--open-artifact (artifact)
  "Open ARTIFACT, enabling live reload for live tool-output artifacts."
  (find-file (plist-get artifact :path))
  (when (eq (plist-get artifact :kind) 'live-tool-output)
    (cond
     ((fboundp 'auto-revert-tail-mode)
      (auto-revert-tail-mode 1))
     ((fboundp 'auto-revert-mode)
      (auto-revert-mode 1)))))

(defun zerostack--append-local-line (text face)
  "Record local TEXT as single-line prompt status.

FACE is accepted for compatibility with older call sites; local notices are not
inserted into the transcript."
  (ignore face)
  (zerostack--set-notice text))

(defun zerostack--ensure-prompt ()
  "Ensure the input prompt exists in the current buffer."
  (unless (and (markerp zerostack--notice-start-marker)
               (markerp zerostack--prompt-start-marker)
               (markerp zerostack--input-marker)
               (markerp zerostack--controls-start-marker)
               (marker-position zerostack--notice-start-marker)
               (marker-position zerostack--prompt-start-marker)
               (marker-position zerostack--input-marker)
               (marker-position zerostack--controls-start-marker))
    (zerostack--without-undo
      (let ((inhibit-read-only t))
        (erase-buffer)
        (setq zerostack--notice-start-marker (copy-marker (point-max) nil))
        (setq zerostack--prompt-start-marker (copy-marker (point-max) nil))
        (set-marker-insertion-type zerostack--notice-start-marker nil)
        (goto-char zerostack--prompt-start-marker)
        (zerostack--insert-prompt-text)
        (set-marker-insertion-type zerostack--prompt-start-marker t)
        (setq zerostack--input-marker (copy-marker (point) nil))
        (setq zerostack--controls-start-marker (copy-marker (point) t))))))

(defun zerostack--insert-prompt-text ()
  "Insert the current prompt text at point."
  (let ((prefix (zerostack--prompt-prefix)))
    (when (not (string-empty-p prefix))
      (insert (propertize (format "%s | " prefix)
                          'face 'zerostack-muted-face
                          'read-only t
                          'front-sticky t
                          'rear-nonsticky t))))
  (insert (propertize (cond
                       ((zerostack--pending-permissions-p)
                        "zs waiting for permission> ")
                       ((and zerostack--loop-active zerostack--thinking)
                        "zs loop thinking> ")
                       (zerostack--loop-active
                        "zs loop> ")
                       (zerostack--thinking
                        "zs thinking> ")
                       (t
                        zerostack-prompt))
                      'face 'zerostack-prompt-face
                      'read-only t
                      'front-sticky t
                      'rear-nonsticky t)))

(defun zerostack--prompt-prefix ()
  "Return prompt metadata prefix."
  (string-join
   (delq nil
         (list zerostack--notice
               zerostack--status
               (and zerostack--reasoning-effort-supported zerostack--reasoning-effort
                    (format "reasoning:%s" zerostack--reasoning-effort))
               (and zerostack--thinking-level
                    (format "thinking:%s" zerostack--thinking-level))
               (and (numberp zerostack--reasoning-tokens)
                    (> zerostack--reasoning-tokens 0)
                    (format "thinking:%s" (zerostack--format-token-count zerostack--reasoning-tokens)))
               zerostack--model
               (and zerostack--subagent-model
                    (format "subagent:%s" zerostack--subagent-model))
               (zerostack--format-token-usage zerostack--tokens zerostack--context-window)))
   " | "))

(defun zerostack--set-status (text)
  "Set durable single-line prompt status TEXT without adding transcript lines."
  (setq zerostack--status (and text (zerostack--status-text text)))
  (when text
    (message "%s" zerostack--status))
  (when (and (markerp zerostack--prompt-start-marker)
             (markerp zerostack--input-marker)
             (marker-position zerostack--prompt-start-marker)
             (marker-position zerostack--input-marker))
    (zerostack--refresh-prompt)))

(defun zerostack--set-notice (text)
  "Set transient single-line prompt notice TEXT without adding transcript lines."
  (when zerostack--notice-timer
    (cancel-timer zerostack--notice-timer)
    (setq zerostack--notice-timer nil))
  (setq zerostack--last-notice text)
  (setq zerostack--notice (and text (zerostack--status-text text)))
  (when text
    (message "%s" zerostack--notice)
    (let ((buffer (current-buffer))
          (notice zerostack--notice))
      (setq zerostack--notice-timer
            (run-at-time
             zerostack-notice-timeout nil
             (lambda (buffer notice)
               (when (buffer-live-p buffer)
                 (with-current-buffer buffer
                   (when (equal zerostack--notice notice)
                     (setq zerostack--notice nil)
                     (setq zerostack--notice-timer nil)
                     (when (and (markerp zerostack--prompt-start-marker)
                                (markerp zerostack--input-marker)
                                (marker-position zerostack--prompt-start-marker)
                                (marker-position zerostack--input-marker))
                       (zerostack--refresh-prompt))))))
             buffer notice))))
  (when (and (markerp zerostack--prompt-start-marker)
             (markerp zerostack--input-marker)
             (marker-position zerostack--prompt-start-marker)
             (marker-position zerostack--input-marker))
    (zerostack--refresh-prompt)))

(defun zerostack--status-text (text)
  "Return TEXT normalized for the single prompt status line."
  (let ((one-line (string-trim
                   (replace-regexp-in-string "[[:space:]]+" " " text))))
    (if (> (length one-line) 140)
        (concat (substring one-line 0 137) "...")
      one-line)))

(defun zerostack--set-thinking (thinking)
  "Set whether the input prompt should indicate THINKING."
  (zerostack--ensure-prompt)
  (when (and thinking zerostack--ready-notify-timer)
    (cancel-timer zerostack--ready-notify-timer)
    (setq zerostack--ready-notify-timer nil))
  (unless (eq zerostack--thinking thinking)
    (let ((became-ready (and zerostack--thinking (not thinking))))
      (setq zerostack--thinking thinking)
      (zerostack--refresh-prompt)
      (when became-ready
        (zerostack--schedule-ready-notify)))))

(defun zerostack--schedule-ready-notify ()
  "Notify ready shortly after completion unless another turn starts."
  (when zerostack--ready-notify-timer
    (cancel-timer zerostack--ready-notify-timer))
  (let ((buffer (current-buffer)))
    (setq zerostack--ready-notify-timer
          (run-at-time
           0.2 nil
           (lambda (buffer)
             (when (buffer-live-p buffer)
               (with-current-buffer buffer
                 (setq zerostack--ready-notify-timer nil)
                 (unless zerostack--thinking
                   (zerostack--notify-ready)))))
           buffer))))

(defun zerostack--notify-ready ()
  "Send a desktop notification that this session is ready for input."
  (zerostack--notify-needs-input "ready"))

(defun zerostack--notify-needs-input (reason)
  "Send a desktop notification that this session needs input for REASON."
  (when (and zerostack-notify-on-ready (executable-find "notify-send"))
    (let ((title (or zerostack--session-title "zerostack")))
      (start-process "zerostack-notify" nil "notify-send" "zerostack" (format "%s needs input: %s" title reason)))))

(defun zerostack--refresh-prompt ()
  "Refresh prompt text while preserving current input."
  (zerostack--without-undo
    (let* ((start (marker-position zerostack--prompt-start-marker))
           (input-start (marker-position zerostack--input-marker))
           (input-end (marker-position zerostack--controls-start-marker))
           (saved-point (copy-marker (point) nil))
           (input-offset (and (>= (point) input-start)
                              (<= (point) input-end)
                              (- (point) input-start)))
           (inhibit-read-only t))
      (unwind-protect
          (progn
            (set-marker-insertion-type zerostack--prompt-start-marker nil)
            (delete-region start input-start)
            (set-marker zerostack--prompt-start-marker start)
            (goto-char start)
            (zerostack--insert-prompt-text)
            (set-marker zerostack--input-marker (point))
            (set-marker-insertion-type zerostack--prompt-start-marker t)
            (if input-offset
                (goto-char (min (marker-position zerostack--controls-start-marker)
                                (+ (marker-position zerostack--input-marker)
                                   input-offset)))
              (goto-char saved-point)))
        (set-marker saved-point nil)))))

(defun zerostack--goto-line-column (line column)
  "Move point to LINE and COLUMN, clamped to the current buffer."
  (goto-char (point-min))
  (forward-line (max 0 (1- line)))
  (move-to-column column))

(defun zerostack--clear-input ()
  "Delete current user input."
  (zerostack--without-undo
    (let ((inhibit-read-only t))
      (delete-region (marker-position zerostack--input-marker)
                     (marker-position zerostack--controls-start-marker))
      (goto-char (marker-position zerostack--controls-start-marker)))))

(defun zerostack--insert-input (text)
  "Insert TEXT at the prompt input area."
  (zerostack--ensure-prompt)
  (zerostack--without-undo
    (let ((inhibit-read-only t))
      (goto-char (marker-position zerostack--controls-start-marker))
      (when (> (point) (marker-position zerostack--input-marker))
        (unless (looking-back "[[:space:]]" (max (point-min) (1- (point))))
          (insert " ")))
      (insert text))))

(defun zerostack--discover-skills ()
  "Return discovered skills as plists for dynamic command selection."
  (let ((seen (make-hash-table :test 'equal))
        skills)
    (dolist (dir (zerostack--skill-search-dirs))
      (dolist (skill (zerostack--skills-in-dir dir))
        (let ((name (plist-get skill :name)))
          (unless (gethash name seen)
            (puthash name t seen)
            (push skill skills)))))
    (nreverse skills)))

(defun zerostack--skill-search-dirs ()
  "Return skill search roots for the current session/worktree."
  (let (dirs)
    (dolist (rel zerostack--home-skill-dirs)
      (push (expand-file-name rel "~") dirs))
    (when-let ((config (getenv "ZS_CONFIG_DIR")))
      (push (expand-file-name "agent/skills" config) dirs))
    (dolist (start (zerostack--skill-start-dirs))
      (dolist (ancestor (zerostack--project-ancestors start))
        (dolist (rel zerostack--project-skill-dirs)
          (push (expand-file-name rel ancestor) dirs))))
    (delete-dups (nreverse dirs))))

(defun zerostack--skill-start-dirs ()
  "Return candidate directories that may identify the current worktree."
  (let (dirs)
    (dolist (dir (list zerostack--worktree-dir zerostack--cwd default-directory))
      (when (and (stringp dir) (not (string-empty-p dir)))
        (push (file-name-as-directory (expand-file-name dir)) dirs)))
    (delete-dups (nreverse dirs))))

(defun zerostack--project-ancestors (&optional start-dir)
  "Return ancestors for START-DIR up to its Git root."
  (let* ((start (file-name-as-directory
                 (expand-file-name (or start-dir
                                       zerostack--worktree-dir
                                       zerostack--cwd
                                       default-directory))))
         (git-root (locate-dominating-file start ".git"))
         (stop (file-name-as-directory (expand-file-name (or git-root start))))
         (current start)
         ancestors done)
    (while (and current (not done))
      (push current ancestors)
      (if (equal (file-truename current) (file-truename stop))
          (setq done t)
        (let ((parent (file-name-directory (directory-file-name current))))
          (if (or (null parent) (equal parent current))
              (setq done t)
            (setq current (file-name-as-directory parent))))))
    (nreverse ancestors)))

(defun zerostack--skills-in-dir (dir)
  "Return skills discovered recursively under DIR."
  (cond
   ((not (file-directory-p dir)) nil)
   ((file-readable-p (expand-file-name "SKILL.md" dir))
    (let ((skill (zerostack--read-skill-file (expand-file-name "SKILL.md" dir))))
      (and skill (list skill))))
   (t
    (let (skills)
      (dolist (entry (sort (directory-files dir t directory-files-no-dot-files-regexp)
                           #'string<))
        (when (and (file-directory-p entry)
                   (not (member (file-name-nondirectory entry) '("node_modules")))
                   (not (string-prefix-p "." (file-name-nondirectory entry))))
          (setq skills (append skills (zerostack--skills-in-dir entry)))))
      skills))))

(defun zerostack--read-skill-file (file)
  "Read one SKILL.md FILE and return a skill plist, or nil."
  (let* ((frontmatter (zerostack--skill-frontmatter file))
         (description (cdr (assoc "description" frontmatter))))
    (when (and description (not (string-empty-p description)))
      (list :name (or (cdr (assoc "name" frontmatter))
                      (file-name-nondirectory
                       (directory-file-name (file-name-directory file))))
            :description description
            :path file
            :disabled (member (downcase (or (cdr (assoc "disable-model-invocation" frontmatter)) ""))
                              '("true" "yes" "1"))))))

(defun zerostack--skill-frontmatter (file)
  "Return simple YAML frontmatter alist from FILE."
  (with-temp-buffer
    (insert-file-contents file nil 0 4096)
    (goto-char (point-min))
    (when (looking-at "---[ \t]*\n")
      (let ((start (match-end 0))
            fields)
        (goto-char start)
        (when (re-search-forward "^---[ \t]*$" nil t)
          (save-restriction
            (narrow-to-region start (match-beginning 0))
            (goto-char (point-min))
            (while (re-search-forward "^\\([^:#\n]+\\):[ \t]*\\(.*\\)$" nil t)
              (push (cons (downcase (string-trim (match-string 1)))
                          (zerostack--strip-yaml-quotes
                           (string-trim (match-string 2))))
                    fields))))
        (nreverse fields)))))

(defun zerostack--strip-yaml-quotes (value)
  "Strip simple surrounding YAML quotes from VALUE."
  (if (and (>= (length value) 2)
           (let ((first (aref value 0))
                 (last (aref value (1- (length value)))))
             (or (and (= first ?\") (= last ?\"))
                 (and (= first ?') (= last ?')))))
      (substring value 1 -1)
    value))

(defun zerostack--face (wire-face)
  "Return Emacs face for WIRE-FACE."
  (or (cdr (assq wire-face zerostack--face-map))
      'zerostack-normal-face))

(defconst zerostack--home-skill-dirs
  '(".config/opencode/skills"
    ".opencode/skills"
    ".claude/skills"
    ".pi/agent/skills"
    ".agents/skills")
  "Home-relative skill directories offered by the command menu.")

(defconst zerostack--project-skill-dirs
  '(".opencode/skills"
    ".claude/skills"
    ".pi/skills"
    ".agents/skills")
  "Project-relative skill directories offered by the command menu.")

(provide 'zerostack)

;;; zerostack.el ends here
