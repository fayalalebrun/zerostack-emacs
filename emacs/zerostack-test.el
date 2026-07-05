;;; zerostack-test.el --- Tests for zerostack.el -*- lexical-binding: t; -*-

;; SPDX-License-Identifier: GPL-3.0-only

;;; Code:

(require 'ert)
(require 'cl-lib)
(require 'zerostack)

(defmacro zerostack-test--with-buffer (&rest body)
  "Run BODY in a temporary `zerostack-mode' buffer."
  (declare (indent 0) (debug t))
  `(let ((buffer (generate-new-buffer " *zerostack-test*")))
     (unwind-protect
         (with-current-buffer buffer
           (zerostack-mode)
           (setq zerostack-auctex-preview nil)
           ,@body)
       (when (buffer-live-p buffer)
         (kill-buffer buffer)))))

(defmacro zerostack-test--with-board-buffer (&rest body)
  "Run BODY in a temporary `zerostack-board-mode' buffer."
  (declare (indent 0) (debug t))
  `(let ((buffer (generate-new-buffer " *zerostack-board-test*")))
     (unwind-protect
         (with-current-buffer buffer
           (zerostack-board-mode)
           ,@body)
       (when (buffer-live-p buffer)
         (kill-buffer buffer)))))

(defun zerostack-test--decode-line (line)
  "Decode one sent protocol LINE."
  (car (read-from-string line)))

(defun zerostack-test--sent-forms (sent)
  "Return SENT protocol lines as decoded forms in send order."
  (mapcar #'zerostack-test--decode-line (nreverse sent)))

(defun zerostack-test--expand-project (path)
  "Mark project PATH expanded in the test board."
  (zerostack-board--set-session-limit (format "project:%s" path) 5))

(defun zerostack-test--wait-until (predicate &optional timeout)
  "Wait until PREDICATE returns non-nil, or fail after TIMEOUT seconds."
  (let ((deadline (+ (float-time) (or timeout 2.0)))
        result)
    (while (and (not (setq result (funcall predicate)))
                (< (float-time) deadline))
      (accept-process-output nil 0.02))
    (unless result
      (ert-fail "timed out waiting for asynchronous condition"))
    result))

(defconst zerostack-test--tool-artifact
  '(:kind tool-output
	  :path "/tmp/zerostack-tool.txt"
	  :mime "text/plain; charset=utf-8"
	  :bytes 17
	  :preview "tool preview"
	  :ephemeral t
	  :expires process-exit))

(defconst zerostack-test--latex-artifact
  '(:kind latex-source
	  :path "/tmp/zerostack-latex.tex"
	  :mime "text/x-tex; charset=utf-8"
	  :bytes 99
	  :preview "\\documentclass{article}"
	  :ephemeral t
	  :expires process-exit))

(defconst zerostack-test--latex-svg-artifact
  '(:kind latex-svg
	  :path "/tmp/zerostack-latex.svg"
	  :mime "image/svg+xml"
	  :bytes 88
	  :preview "<svg"
	  :ephemeral t
	  :expires process-exit))

(defconst zerostack-test--latex-item
  `(:id "turn-1-latex-1"
	:display nil
	:source "x^2"
	:line-start 2
	:col-start 2
	:line-end 2
	:col-end 8
	:artifact ,zerostack-test--latex-artifact
	:svg-artifact ,zerostack-test--latex-svg-artifact))

(defconst zerostack-test--board-snapshot
  '(zerostack-board
    :version 1
    :provider "openai-codex"
    :model "gpt-5.5"
    :subagent-provider "openrouter"
    :subagent-model "deepseek/deepseek-chat-v3.1"
    :projects
    ((:name "live-repo"
	    :path "/repo/live"
	    :repo "/repo/live/.git"
	    :alive t
	    :updated-at "2026-06-20T00:00:00Z"
	    :worktrees
	    ((:path "/repo/live-wt"
		    :branch "feature"
		    :description "feature work"
		    :alive t
		    :sessions
		    ((:id "live-session"
			  :short-id "live-ses"
			  :title "Live session"
			  :cwd "/repo/live-wt"
			  :model "model"
			  :provider "provider"
			  :created-at "2026-06-20T00:00:00Z"
			  :updated-at "2026-06-20T00:00:00Z"
			  :message-count 2
			  :tokens 10
			  :context-window 100
			  :cost 0.1
			  :alive t
			  :pid 123
			  :socket "/tmp/live.sock")
		     (:id "dead-session"
			  :short-id "dead-ses"
			  :title "Dead session"
			  :cwd "/repo/live-wt"
			  :model "model"
			  :provider "provider"
			  :created-at "2026-06-19T00:00:00Z"
			  :updated-at "2026-06-19T00:00:00Z"
			  :message-count 1
			  :tokens 5
			  :context-window 100
			  :cost 0.0
			  :alive nil
			  :pid nil
			  :socket nil)))
	     (:path "/repo/live-other"
		    :branch "main"
		    :description ""
		    :alive nil
		    :sessions nil)))
     (:name "dead-repo"
	    :path "/repo/dead"
	    :repo "/repo/dead/.git"
	    :alive nil
	    :updated-at "2026-06-18T00:00:00Z"
	    :worktrees
	    ((:path "/repo/dead"
		    :branch "main"
		    :description ""
		    :alive nil
		    :sessions
		    ((:id "old-session"
			  :short-id "old-sess"
			  :title "Old session"
			  :cwd "/repo/dead"
			  :model "model"
			  :provider "provider"
			  :created-at "2026-06-18T00:00:00Z"
			  :updated-at "2026-06-18T00:00:00Z"
			  :message-count 1
			  :tokens 1
			  :context-window 100
			  :cost 0.0
			  :alive nil
			  :pid nil
			  :socket nil))))))
    :loose-workspaces
    ((:path "/nongit/work"
	    :alive nil
	    :updated-at "2026-06-17T00:00:00Z"
	    :sessions
	    ((:id "loose-session"
		  :short-id "loose-se"
		  :title "Loose session"
		  :cwd "/nongit/work"
		  :model "model"
		  :provider "provider"
		  :created-at "2026-06-17T00:00:00Z"
		  :updated-at "2026-06-17T00:00:00Z"
		  :message-count 1
		  :tokens 1
		  :context-window 100
		  :cost 0.0
		  :alive nil
		  :pid nil
		  :socket nil))))))

(ert-deftest zerostack-test-board-refresh-renders-tree ()
  (zerostack-test--with-board-buffer
   (zerostack-test--expand-project "/repo/live")
   (setq-local zerostack-board--fetch-function
               (lambda () zerostack-test--board-snapshot))
   (zerostack-board-refresh)
   (should (equal zerostack-board--snapshot zerostack-test--board-snapshot))
   (let ((text (buffer-string)))
     (should (string-match-p "\\* project live-repo" text))
     (should (string-match-p "  \\* feature work  feature  live-wt/" text))
     (should-not (string-match-p "Live session" text))
     (should (string-match-p "    (no description)  main  live-other/" text))
     (should (string-match-p "other workspaces" text))
     (should (string-match-p "workspace  work/  /nongit/work" text))
     (should (string-match-p "Loose session" text))
     (should (< (string-match "project live-repo" text)
                (string-match "project dead-repo" text))))
   (goto-char (point-min))
   (search-forward "live-wt/")
   (let* ((item (get-text-property (1- (point)) 'zerostack-board-item))
          (session (plist-get item :session-item)))
     (should (eq (plist-get item :type) 'worktree))
     (should (equal (plist-get session :socket) "/tmp/live.sock")))
   (should (eq (get-text-property (1- (point)) 'face) 'zerostack-board-alive-face))
   (goto-char (point-min))
   (search-forward "project live-repo")
   (should (eq (get-text-property (point) 'face) 'zerostack-board-alive-face))))

(ert-deftest zerostack-test-board-renders-needs-attention-and-dismisses ()
  (zerostack-test--with-board-buffer
   (let* ((attention (zerostack-test--session-plist "attention-session" "Ready session" "2026-06-21T00:00:00Z" t))
          (snapshot `(zerostack-board
                      :version 1
                      :needs-attention (,attention)
                      :projects nil
                      :loose-workspaces nil))
          dismissed refreshed)
     (cl-letf (((symbol-function 'call-process)
                (lambda (program _infile _buffer _display &rest args)
                  (push (cons program args) dismissed)
                  0))
               ((symbol-function 'yes-or-no-p)
                (lambda (&rest _) (error "should not confirm attention dismiss")))
               ((symbol-function 'zerostack-board-refresh)
                (lambda () (setq refreshed t))))
       (zerostack-board--render snapshot)
       (let ((text (buffer-string)))
         (should (string-match-p "needs attention" text))
         (should (string-match-p "Ready session" text))
         (should (string-match-p "/repo/many" text))
         (should (string-match-p "dismiss" text)))
       (goto-char (point-min))
       (search-forward "dismiss")
       (push-button (button-at (1- (point))))
       (goto-char (point-min))
       (search-forward "Ready session")
       (zerostack-board-stop-at-point)
       (goto-char (point-min))
       (search-forward "Ready session")
       (zerostack-board-trash-at-point)
        (should (equal dismissed
                       (make-list 3 (cons zerostack-command '("--emacs-dismiss-attention" "attention-session")))))
        (should refreshed)))))

(ert-deftest zerostack-test-board-jump-uses-completion ()
  (zerostack-test--with-board-buffer
   (zerostack-test--expand-project "/repo/live")
   (zerostack-board--render zerostack-test--board-snapshot)
   (let (choices)
     (cl-letf (((symbol-function 'completing-read)
                (lambda (_prompt collection &rest _)
                  (setq choices (mapcar #'car collection))
                  (cl-find-if (lambda (choice) (string-match-p "Loose session" choice)) choices))))
       (zerostack-board-jump))
     (should (cl-some (lambda (choice) (string-match-p "project: .*live-repo" choice)) choices))
     (should (cl-some (lambda (choice) (string-match-p "session: .*Loose session" choice)) choices))
     (should (looking-at "    .*Loose session")))))

(ert-deftest zerostack-test-board-open-attention-dismisses-and-opens ()
  (zerostack-test--with-board-buffer
   (let* ((attention (zerostack-test--session-plist "attention-session" "Ready session" "2026-06-21T00:00:00Z" t))
          (snapshot `(zerostack-board
                      :version 1
                      :needs-attention (,attention)
                      :projects nil
                      :loose-workspaces nil))
          dismissed
          opened)
     (cl-letf (((symbol-function 'completing-read)
                (lambda (_prompt collection &rest _)
                  (caar collection)))
               ((symbol-function 'call-process)
                (lambda (program _infile _buffer _display &rest args)
                  (push (cons program args) dismissed)
                  0))
               ((symbol-function 'zerostack-board-refresh)
                (lambda () nil))
               ((symbol-function 'zerostack-board--open-session)
                (lambda (item) (setq opened item))))
       (zerostack-board--render snapshot)
       (zerostack-board-open-attention)
       (should (equal dismissed
                      (list (cons zerostack-command '("--emacs-dismiss-attention" "attention-session")))))
       (should (equal (plist-get opened :id) "attention-session"))
       (should (plist-get opened :attention))))))

(ert-deftest zerostack-test-board-colors-open-thinking-session-yellow ()
  (let ((chat (generate-new-buffer " *zerostack-open-session*")))
    (unwind-protect
        (progn
          (with-current-buffer chat
            (zerostack-mode)
            (setq zerostack--session "live-session"
                  zerostack--thinking t
                  zerostack--tokens 42
                  zerostack--context-window 100))
          (zerostack-test--with-board-buffer
           (zerostack-test--expand-project "/repo/live")
           (zerostack-board--render zerostack-test--board-snapshot)
           (goto-char (point-min))
           (search-forward "live-wt/")
           (should (eq (get-text-property (1- (point)) 'face)
                       'zerostack-board-thinking-face))
           (should (string-match-p "feature work  feature  live-wt/" (buffer-string)))))
      (when (buffer-live-p chat)
        (kill-buffer chat)))))

(defun zerostack-test--session-plist (id title updated &optional alive)
  "Return a board session plist for tests."
  `(:id ,id
	:short-id ,(substring id 0 (min 8 (length id)))
	:title ,title
	:cwd "/repo/many"
	:model "model"
	:provider "provider"
	:created-at ,updated
	:updated-at ,updated
	:message-count 1
	:tokens 1
	:context-window 100
	:cost 0.0
	:alive ,alive
	:pid ,(and alive 123)
	:socket ,(and alive (format "/tmp/%s.sock" id))))

(ert-deftest zerostack-test-board-paginates-session-lists ()
  (zerostack-test--with-board-buffer
   (let* ((sessions (list
                     (zerostack-test--session-plist "session-live" "Live first" "2026-06-14T00:00:00Z" t)
                     (zerostack-test--session-plist "session-live-2" "Live second" "2026-06-13T12:00:00Z" t)
                     (zerostack-test--session-plist "session-1" "Recent one" "2026-06-13T00:00:00Z")
                     (zerostack-test--session-plist "session-2" "Recent two" "2026-06-12T00:00:00Z")
                     (zerostack-test--session-plist "session-3" "Recent three" "2026-06-11T00:00:00Z")
                     (zerostack-test--session-plist "session-4" "Recent four" "2026-06-10T00:00:00Z")
                     (zerostack-test--session-plist "session-5" "Hidden five" "2026-06-09T00:00:00Z")
                     (zerostack-test--session-plist "session-6" "Hidden six" "2026-06-08T00:00:00Z")))
          (snapshot `(zerostack-board
                      :version 1
                      :projects
                      ((:name "many-repo"
                              :path "/repo/many"
                              :repo "/repo/many/.git"
                              :alive t
                              :updated-at "2026-06-14T00:00:00Z"
                              :worktrees
                              ((:path "/repo/many"
				      :branch "main"
				      :description "many sessions"
				      :alive t
				      :sessions ,sessions))))
                      :loose-workspaces nil)))
     (zerostack-test--expand-project "/repo/many")
     (setq-local zerostack-board--snapshot snapshot)
     (zerostack-board--render snapshot)
     (let ((text (buffer-string)))
       (should (string-match-p "Live first" text))
       (should (string-match-p "Recent three" text))
       (should-not (string-match-p "Recent four" text))
       (should-not (string-match-p "Hidden five" text))
       (should (string-match-p "show 5 more (3 remaining)" text))
       (should (< (string-match "many/" text)
                  (string-match "Recent one" text))))
     (goto-char (point-min))
     (search-forward "show 5 more")
     (zerostack-board-open-at-point)
     (let ((text (buffer-string)))
       (should (string-match-p "Hidden five" text))
       (should (string-match-p "Hidden six" text))
       (should-not (string-match-p "show 5 more" text))))))

(ert-deftest zerostack-test-board-single-active-workspace-colors-entire-workspace-row ()
  (zerostack-test--with-board-buffer
   (let* ((session (zerostack-test--session-plist "workspace-live" "Live workspace session" "2026-06-14T00:00:00Z" t))
          (snapshot `(zerostack-board
                      :version 1
                      :projects nil
                      :loose-workspaces
                      ((:path "/nongit/single" :alive t :sessions (,session))))))
     (zerostack-board--render snapshot)
     (goto-char (point-min))
     (search-forward "* workspace")
     (should (eq (get-text-property (point) 'face)
                 'zerostack-board-alive-face))
     (search-forward "/nongit/single")
     (should (eq (get-text-property (1- (point)) 'face)
                 'zerostack-board-alive-face))
     (should-not (string-match-p "Live workspace session" (buffer-string))))))

(ert-deftest zerostack-test-board-single-active-worktree-colors-entire-worktree-row ()
  (zerostack-test--with-board-buffer
   (let* ((session (zerostack-test--session-plist "worktree-live" "Live worktree session" "2026-06-14T00:00:00Z" t))
          (snapshot `(zerostack-board
                      :version 1
                      :projects
                      ((:path "/repo" :repo "/repo" :name "repo" :alive t
                              :worktrees ((:path "/repo/wt" :branch "main" :description "main work"
						 :alive t :sessions (,session)))))
                      :loose-workspaces nil)))
     (zerostack-test--expand-project "/repo")
     (zerostack-board--render snapshot)
     (goto-char (point-min))
     (search-forward "main work")
     (should (eq (get-text-property (1- (point)) 'face)
                 'zerostack-board-alive-face))
     (search-forward "wt/")
     (should (eq (get-text-property (1- (point)) 'face)
                 'zerostack-board-alive-face))
     (should-not (string-match-p "Live worktree session" (buffer-string))))))

(ert-deftest zerostack-test-board-single-active-project-collapses-workspace-row ()
  (zerostack-test--with-board-buffer
   (let* ((live-session (zerostack-test--session-plist "project-live" "Live project session" "2026-06-14T00:00:00Z" t))
          (old-session (zerostack-test--session-plist "project-old" "Old project session" "2026-06-13T00:00:00Z" nil))
          (snapshot `(zerostack-board
                      :version 1
                      :projects
                      ((:path "/repo" :repo "/repo" :name "repo" :alive t
                              :worktrees ((:path "/repo/live" :branch "live" :description "live work"
						 :alive t :sessions (,live-session ,old-session))
					  (:path "/repo/old" :branch "old" :description "old work"
						 :alive nil :sessions nil)
					  (:path "/repo/old2" :branch "old2" :description "old work 2"
						 :alive nil :sessions nil)
					  (:path "/repo/old3" :branch "old3" :description "old work 3"
						 :alive nil :sessions nil)
					  (:path "/repo/old4" :branch "old4" :description "old work 4"
						 :alive nil :sessions nil)
					  (:path "/repo/old5" :branch "old5" :description "old work 5"
						 :alive nil :sessions nil))))
                      :loose-workspaces nil)))
     (setq-local zerostack-board--snapshot snapshot)
     (zerostack-board--render snapshot)
     (let ((text (buffer-string)))
       (should (string-match-p "project repo  /repo  \\+ show 5 more" text))
       (should-not (string-match-p "live work" text))
       (should-not (string-match-p "Live project session" text)))
     (goto-char (point-min))
     (search-forward "project repo")
     (should (eq (get-text-property (1- (point)) 'face)
                 'zerostack-board-alive-face))
     (search-forward "show 5 more")
     (zerostack-board-open-at-point)
     (let ((text (buffer-string)))
       (should (string-match-p "live work" text))
       (should (string-match-p "old work" text))
       (should (string-match-p "live work  live  live/  /repo/live  \\+ show 5 more" text))
       (should-not (string-match-p "Live project session" text))
       (should-not (string-match-p "project repo  /repo  \\+ show 5 more" text))))))

(ert-deftest zerostack-test-board-single-active-workspace-load-more-is-inline ()
  (zerostack-test--with-board-buffer
   (let* ((sessions (list
                     (zerostack-test--session-plist "workspace-live" "Live first" "2026-06-14T00:00:00Z" t)
                     (zerostack-test--session-plist "workspace-1" "Recent one" "2026-06-13T00:00:00Z")
                     (zerostack-test--session-plist "workspace-2" "Recent two" "2026-06-12T00:00:00Z")
                     (zerostack-test--session-plist "workspace-3" "Recent three" "2026-06-11T00:00:00Z")
                     (zerostack-test--session-plist "workspace-4" "Recent four" "2026-06-10T00:00:00Z")
                     (zerostack-test--session-plist "workspace-5" "Hidden five" "2026-06-09T00:00:00Z")
                     (zerostack-test--session-plist "workspace-6" "Hidden six" "2026-06-08T00:00:00Z")))
          (snapshot `(zerostack-board
                      :version 1
                      :projects nil
                      :loose-workspaces
                      ((:path "/nongit/many" :alive t :sessions ,sessions)))))
     (setq-local zerostack-board--snapshot snapshot)
     (zerostack-board--render snapshot)
     (let ((text (buffer-string)))
       (should (string-match-p "workspace  many/  /nongit/many  \\+ show 5 more" text))
       (should-not (string-match-p "Recent one" text))
       (should-not (string-match-p "^    + show 5 more" text))
       (should-not (string-match-p "Hidden five" text)))
     (goto-char (point-min))
     (search-forward "show 5 more")
     (let ((item (get-text-property (point) 'zerostack-board-item)))
       (should (eq (plist-get item :type) 'load-more)))
     (zerostack-board-open-at-point)
     (let ((text (buffer-string)))
       (should (string-match-p "Recent one" text))
       (should-not (string-match-p "Hidden five" text))
       (should (string-match-p (regexp-quote "    + show 5 more") text))
       (should-not (string-match-p "workspace  many/  /nongit/many  \\+ show 5 more" text))))))

(ert-deftest zerostack-test-board-renders-pinned-directory ()
  (let* ((dir (make-temp-file "zerostack-pinned" t))
         (zerostack-board-directories (list dir)))
    (unwind-protect
        (zerostack-test--with-board-buffer
         (zerostack-board--render
          '(zerostack-board :version 1 :projects nil :loose-workspaces nil))
         (let ((text (buffer-string))
               (normalized (zerostack-board--normalize-directory dir)))
           (should (string-match-p "other workspaces" text))
           (should (string-match-p (regexp-quote normalized) text))
           (should-not (string-match-p "no saved sessions" text))))
      (delete-directory dir t))))

(ert-deftest zerostack-test-board-dedupes-pinned-directories-from-snapshot ()
  (let ((zerostack-board-directories '("/nongit/work/" "/repo/live-wt" "/new/work")))
    (let ((pinned (zerostack-board--pinned-workspaces zerostack-test--board-snapshot)))
      (should (equal (mapcar (lambda (workspace) (plist-get workspace :path)) pinned)
                     '("/new/work")))
      (should (zerostack-board--snapshot-has-directory-p
               zerostack-test--board-snapshot "/repo/live-wt/")))))

(ert-deftest zerostack-test-board-add-current-directory-uses-projectile-root ()
  (let* ((dir (make-temp-file "zerostack-projectile" t))
         (zerostack-board-buffer-name " *zerostack-board-add-test*")
         (zerostack-board-directories nil))
    (unwind-protect
        (cl-letf (((symbol-function 'projectile-project-root)
                   (lambda () dir))
                  ((symbol-function 'zerostack-board--fetch)
                   (lambda ()
                     '(zerostack-board :version 1 :projects nil :loose-workspaces nil)))
                  ((symbol-function 'pop-to-buffer)
                   (lambda (buffer &rest _) buffer)))
          (zerostack-board-add-current-directory)
          (should (equal zerostack-board-directories
                         (list (zerostack-board--normalize-directory dir))))
          (with-current-buffer zerostack-board-buffer-name
            (should (string-match-p
                     (regexp-quote (zerostack-board--normalize-directory dir))
                     (buffer-string)))))
      (when (get-buffer zerostack-board-buffer-name)
        (kill-buffer zerostack-board-buffer-name))
      (delete-directory dir t))))

(ert-deftest zerostack-test-global-board-keybind ()
  (should (eq (lookup-key zerostack-global-mode-map (kbd "C-c z"))
              #'zerostack-board-add-current-directory)))


(ert-deftest zerostack-test-board-create-session-from-pinned-workspace ()
  (let* ((dir (make-temp-file "zerostack-pinned-session" t))
         (zerostack-board-directories (list dir)))
    (unwind-protect
        (zerostack-test--with-board-buffer
         (zerostack-board--render
          '(zerostack-board :version 1 :projects nil :loose-workspaces nil))
         (let (started start-directory)
           (cl-letf (((symbol-function 'zerostack)
                      (lambda (args &rest _)
                        (setq started args)
                        (setq start-directory default-directory))))
             (goto-char (point-min))
             (search-forward (file-name-nondirectory dir))
             (beginning-of-line)
             (zerostack-board-create-at-point)
             (should (equal started nil))
             (should (equal start-directory (file-name-as-directory
                                             (zerostack-board--normalize-directory dir)))))))
      (delete-directory dir t))))

(ert-deftest zerostack-test-board-refresh-preserves-cursor-position ()
  (zerostack-test--with-board-buffer
   (zerostack-test--expand-project "/repo/live")
   (setq-local zerostack-board--fetch-function
               (lambda () zerostack-test--board-snapshot))
   (zerostack-board-refresh)
   (goto-char (point-min))
   (search-forward "feature work")
   (let ((line (line-number-at-pos))
         (column (current-column)))
     (zerostack-board-refresh)
     (should (= (line-number-at-pos) line))
     (should (= (current-column) column))
     (should (looking-back "feature work" (line-beginning-position))))))

(ert-deftest zerostack-test-board-open-session-actions ()
  (zerostack-test--with-board-buffer
   (zerostack-test--expand-project "/repo/live")
   (zerostack-board--render zerostack-test--board-snapshot)
   (let (connected connected-title connected-cwd connected-worktree connected-session
                   started started-title started-cwd started-worktree started-session)
     (cl-letf (((symbol-function 'zerostack-connect)
                (lambda (socket &optional title cwd worktree session-id)
                  (setq connected socket
                        connected-title title
                        connected-cwd cwd
                        connected-worktree worktree
                        connected-session session-id)))
               ((symbol-function 'zerostack)
                (lambda (args &optional title cwd worktree session-id)
                  (setq started args
                        started-title title
                        started-cwd cwd
                        started-worktree worktree
                        started-session session-id))))
       (goto-char (point-min))
       (search-forward "live-wt/")
       (beginning-of-line)
       (zerostack-board-open-at-point)
       (should (equal connected "/tmp/live.sock"))
       (should (equal connected-title "Live session"))
       (should (equal connected-cwd "/repo/live-wt"))
       (should (equal connected-worktree "/repo/live-wt"))
       (should (equal connected-session "live-session"))

       (zerostack-board--set-session-limit "worktree:/repo/live-wt" 5)
       (zerostack-board--render zerostack-test--board-snapshot)
       (goto-char (point-min))
       (search-forward "Dead session")
       (beginning-of-line)
       (zerostack-board-open-at-point)
       (should (equal started '("--session" "dead-session")))
       (should (equal started-title "Dead session"))
       (should (equal started-cwd "/repo/live-wt"))
       (should (equal started-worktree "/repo/live-wt"))
       (should (equal started-session "dead-session"))))))

(ert-deftest zerostack-test-board-open-live-session-uses-chat-buffer ()
  (zerostack-test--with-board-buffer
   (zerostack-test--expand-project "/repo/live")
   (zerostack-board--render zerostack-test--board-snapshot)
   (let ((board-buffer (current-buffer))
         connected-socket chat-buffer popped-buffer)
     (cl-letf (((symbol-function 'zerostack--connect-buffer)
                (lambda (socket)
                  (setq connected-socket socket)
                  (setq chat-buffer (current-buffer))))
               ((symbol-function 'pop-to-buffer)
                (lambda (buffer &rest _)
                  (setq popped-buffer buffer)
                  buffer)))
       (goto-char (point-min))
       (search-forward "live-wt/")
       (beginning-of-line)
       (unwind-protect
           (let ((result (zerostack-board-open-at-point)))
             (should (equal connected-socket "/tmp/live.sock"))
             (should (buffer-live-p chat-buffer))
             (should (eq result chat-buffer))
             (should (eq popped-buffer chat-buffer))
             (should-not (eq board-buffer chat-buffer))
             (should (buffer-local-value 'zerostack--input-marker chat-buffer))
             (should (string-match-p "\\*zerostack: Live session @ live-wt"
                                     (buffer-name chat-buffer)))
             (should (derived-mode-p 'zerostack-board-mode)))
         (when (buffer-live-p chat-buffer)
           (kill-buffer chat-buffer)))))))

(ert-deftest zerostack-test-session-metadata-updates-default-directory ()
  (let* ((root (make-temp-file "zerostack-root" t))
         (subdir (expand-file-name "subdir" root)))
    (unwind-protect
        (progn
          (make-directory subdir)
          (zerostack-test--with-buffer
           (let ((default-directory temporary-file-directory))
             (zerostack--set-session-metadata "Title" subdir root)
             (should (equal default-directory (file-name-as-directory root))))))
      (delete-directory root t))))

(ert-deftest zerostack-test-session-metadata-ignores_missing_default_directory ()
  (zerostack-test--with-buffer
   (let ((default-directory temporary-file-directory))
     (zerostack--set-session-metadata "Title" "/no/such/cwd" "/no/such/root")
     (should (equal default-directory temporary-file-directory)))))

(ert-deftest zerostack-test-connect-reuses-buffer-for-session ()
  (let (connected popped)
    (cl-letf (((symbol-function 'zerostack--connect-buffer)
               (lambda (socket)
                 (push (cons (current-buffer) socket) connected)
                 (setq zerostack--socket socket)))
              ((symbol-function 'pop-to-buffer)
               (lambda (buffer &rest _)
                 (setq popped buffer)
                 buffer)))
      (let ((buffer (zerostack-connect "/tmp/live.sock"
                                       "Live session"
                                       "/repo/live-wt"
                                       "/repo/live-wt"
                                       "live-session")))
        (unwind-protect
            (let ((again (zerostack-connect "/tmp/live.sock"
                                            "Live session"
                                            "/repo/live-wt"
                                            "/repo/live-wt"
                                            "live-session")))
              (should (eq again buffer))
              (should (eq popped buffer))
              (should (= (length (delete-dups (mapcar #'car connected))) 1))
              (should (equal (buffer-local-value 'zerostack--session buffer)
                             "live-session")))
          (when (buffer-live-p buffer)
            (kill-buffer buffer)))))))

(ert-deftest zerostack-test-start-reuses-buffer-for-session-arg ()
  (let (starts popped)
    (cl-letf (((symbol-function 'zerostack--start-server)
               (lambda (args)
                 (push (cons (current-buffer) args) starts)))
              ((symbol-function 'pop-to-buffer)
               (lambda (buffer &rest _)
                 (setq popped buffer)
                 buffer)))
      (let ((buffer (zerostack '("--session" "dead-session")
                               "Dead session"
                               "/repo/live-wt"
                               "/repo/live-wt")))
        (unwind-protect
            (let ((again (zerostack '("--session" "dead-session")
                                    "Dead session"
                                    "/repo/live-wt"
                                    "/repo/live-wt")))
              (should (eq again buffer))
              (should (eq popped buffer))
              (should (= (length (delete-dups (mapcar #'car starts))) 1))
              (should (equal (buffer-local-value 'zerostack--session buffer)
                             "dead-session")))
          (when (buffer-live-p buffer)
            (kill-buffer buffer)))))))

(ert-deftest zerostack-test-startup-error-surfaces-stderr ()
  (let ((script (make-temp-file "zerostack-fail" nil nil
                                "#!/bin/sh\nprintf '%s\n' 'Error: missing key' >&2\nexit 1\n"))
        (zerostack-notice-timeout 5.0)
        process
        stderr-buffer)
    (set-file-modes script #o700)
    (unwind-protect
        (zerostack-test--with-buffer
         (let ((zerostack-command script))
           (zerostack--start-server nil)
           (setq process zerostack--server-process)
           (setq stderr-buffer (process-get process 'zerostack-stderr-buffer))
           (zerostack-test--wait-until
            (lambda ()
              (and (not (process-live-p process))
                   (not zerostack--startup-timer)
                   zerostack--notice
                   (string-match-p "Error: missing key" zerostack--notice))))
           (should (string-match-p "server .*Error: missing key" zerostack--notice))
           (should (equal zerostack--last-notice zerostack--notice))
           (should-not zerostack--status)
           (should-not zerostack--startup-timer)
           (should-not zerostack--server-process)))
      (when (and process (process-live-p process))
        (delete-process process))
      (when (buffer-live-p stderr-buffer)
        (kill-buffer stderr-buffer))
      (delete-file script))))

(ert-deftest zerostack-test-ready-removes-stale-duplicate-session-buffer ()
  (let ((stale (generate-new-buffer " *zerostack-stale*"))
        (current (generate-new-buffer " *zerostack-current*")))
    (unwind-protect
        (progn
          (with-current-buffer stale
            (zerostack-mode)
            (setq zerostack--session "same-session"))
          (with-current-buffer current
            (zerostack-mode)
            (zerostack--handle-ready
             '(:protocol 1 :session "same-session" :pid 123 :socket "/tmp/same.sock"))
            (should (equal zerostack--session "same-session")))
          (should-not (buffer-live-p stale))
          (should (buffer-live-p current)))
      (when (buffer-live-p stale)
        (kill-buffer stale))
      (when (buffer-live-p current)
        (kill-buffer current)))))

(ert-deftest zerostack-test-board-create-actions ()
  (zerostack-test--with-board-buffer
   (zerostack-test--expand-project "/repo/live")
   (zerostack-board--render zerostack-test--board-snapshot)
   (let (git-calls refreshed started started-called start-directory read-answers)
     (setq read-answers '("feature/new-board" "Created from board"))
     (cl-letf (((symbol-function 'read-string)
                (lambda (&rest _)
                  (pop read-answers)))
               ((symbol-function 'read-file-name)
                (lambda (&rest _)
                  "/repo/new-board"))
               ((symbol-function 'zerostack-board--call-git)
                (lambda (dir &rest args)
                  (push (cons dir args) git-calls)))
               ((symbol-function 'zerostack-board-refresh)
                (lambda () (setq refreshed t)))
               ((symbol-function 'file-directory-p)
                (lambda (path) (member path '("/repo/live-wt" "/repo/live"))))
               ((symbol-function 'zerostack)
                (lambda (args)
                  (setq started-called t)
                  (setq started args)
                  (setq start-directory default-directory))))
       (goto-char (point-min))
       (search-forward "project live-repo")
       (beginning-of-line)
       (zerostack-board-create-at-point)
       (should refreshed)
       (should (member '("/repo/live" "worktree" "add" "-b" "feature/new-board" "/repo/new-board")
                       git-calls))
       (should (member '("/repo/live" "config" "branch.feature/new-board.description" "Created from board")
                       git-calls))

       (goto-char (point-min))
       (search-forward "feature work")
       (beginning-of-line)
       (zerostack-board-create-at-point)
       (should started-called)
       (should (equal started nil))
       (should (equal start-directory "/repo/live-wt/"))))))

(ert-deftest zerostack-test-board-trash-actions ()
  (zerostack-test--with-board-buffer
   (zerostack-test--expand-project "/repo/live")
   (zerostack-board--render zerostack-test--board-snapshot)
   (let ((process-environment (cons "ZS_DATA_DIR=/data" process-environment))
         trashed git-calls refreshes)
     (cl-letf (((symbol-function 'yes-or-no-p) (lambda (&rest _) t))
               ((symbol-function 'file-directory-p)
                (lambda (path) (member path '("/repo/live-wt" "/repo/live"))))
               ((symbol-function 'file-exists-p)
                (lambda (path) (equal path "/data/sessions/dead-session.json")))
               ((symbol-function 'zerostack-board--trash-path)
                (lambda (path) (push path trashed)))
               ((symbol-function 'zerostack-board--call-git)
                (lambda (dir &rest args)
                  (push (cons dir args) git-calls)))
               ((symbol-function 'zerostack-board-refresh)
                (lambda () (setq refreshes (1+ (or refreshes 0))))))
       (goto-char (point-min))
       (search-forward "feature work")
       (beginning-of-line)
       (zerostack-board-trash-at-point)
       (should (member "/repo/live-wt" trashed))
       (should (member '("/repo/live" "worktree" "prune") git-calls))

       (zerostack-board--set-session-limit "worktree:/repo/live-wt" 5)
       (zerostack-board--render zerostack-test--board-snapshot)
       (goto-char (point-min))
       (search-forward "Dead session")
       (beginning-of-line)
       (zerostack-board-trash-at-point)
       (should (member "/data/sessions/dead-session.json" trashed))
       (should (= refreshes 2))))))

(ert-deftest zerostack-test-board-set-branch-description ()
  (zerostack-test--with-board-buffer
   (zerostack-test--expand-project "/repo/live")
   (zerostack-board--render zerostack-test--board-snapshot)
   (let (git-calls refreshed)
     (cl-letf (((symbol-function 'read-string) (lambda (&rest _) "new description"))
               ((symbol-function 'zerostack-board--call-git)
                (lambda (dir &rest args)
                  (push (cons dir args) git-calls)))
               ((symbol-function 'zerostack-board-refresh)
                (lambda () (setq refreshed t))))
       (goto-char (point-min))
       (search-forward "feature work")
       (beginning-of-line)
       (zerostack-board-set-description-at-point)
       (should (equal git-calls
                      '(("/repo/live" "config" "branch.feature.description" "new description"))))
       (should refreshed)))))

(ert-deftest zerostack-test-board-stop-live-session ()
  (zerostack-test--with-board-buffer
   (zerostack-test--expand-project "/repo/live")
   (zerostack-board--render zerostack-test--board-snapshot)
   (let ((chat-buffer (generate-new-buffer " *zerostack-live-session*"))
         signals refreshed)
     (unwind-protect
         (progn
           (with-current-buffer chat-buffer
             (zerostack-mode)
             (setq zerostack--session "live-session")
             (setq zerostack--socket "/tmp/live.sock"))
           (cl-letf (((symbol-function 'yes-or-no-p) (lambda (&rest _) t))
                     ((symbol-function 'signal-process)
                      (lambda (pid signal) (push (list pid signal) signals)))
                     ((symbol-function 'run-at-time)
                      (lambda (&rest args)
                        (let ((fn (nth 2 args))
                              (buffer (nth 3 args)))
                          (with-current-buffer buffer
                            (funcall fn buffer)))))
                     ((symbol-function 'zerostack-board-refresh)
                      (lambda () (setq refreshed t))))
             (goto-char (point-min))
             (search-forward "live-wt/")
             (beginning-of-line)
             (zerostack-board-stop-at-point)
             (should (equal signals '((123 term))))
             (should-not (buffer-live-p chat-buffer))
             (should refreshed)))
       (when (buffer-live-p chat-buffer)
         (kill-buffer chat-buffer))))))

(ert-deftest zerostack-test-board-default-provider-model-actions ()
  (zerostack-test--with-board-buffer
   (let ((choices '("openai-codex" "gpt-5.5" "openrouter" "deepseek/deepseek-chat-v3.1"))
         calls
         (refreshes 0))
     (cl-letf (((symbol-function 'completing-read)
                (lambda (&rest _) (pop choices)))
               ((symbol-function 'zerostack-board-refresh)
                (lambda () (cl-incf refreshes))))
       (let ((zerostack--config-command-function
              (lambda (&rest args)
                (push args calls)
                (pcase args
                  ('("providers") "anthropic\nopenai-codex\nopenrouter\n")
                  ('("models") "gpt-5.5\ngpt-5.1\n")
                  ('("set-provider" "openai-codex") "provider openai-codex\nmodel gpt-5.5\n")
                  ('("set-model" "gpt-5.5") "provider openai-codex\nmodel gpt-5.5\n")
                  ('("set-subagent-provider" "openrouter") "subagent_provider openrouter\nsubagent_model deepseek/deepseek-chat-v3.1\n")
                  ('("set-subagent-model" "deepseek/deepseek-chat-v3.1") "subagent_provider openrouter\nsubagent_model deepseek/deepseek-chat-v3.1\n")
                  (_ (error "unexpected config args: %S" args))))))
         (zerostack-board-set-default-provider)
         (zerostack-board-set-default-model)
         (zerostack-board-set-default-subagent-provider)
         (zerostack-board-set-default-subagent-model)))
     (should (equal (nreverse calls)
                    '(("providers")
                      ("set-provider" "openai-codex")
                      ("models")
                      ("set-model" "gpt-5.5")
                      ("providers")
                      ("set-subagent-provider" "openrouter")
                      ("models")
                      ("set-subagent-model" "deepseek/deepseek-chat-v3.1"))))
     (should (= refreshes 4)))))

(ert-deftest zerostack-test-board-renders-provider-model-buttons ()
  (zerostack-test--with-board-buffer
   (zerostack-board--render zerostack-test--board-snapshot)
   (goto-char (point-min))
   (should (search-forward "Main: " nil t))
   (should (search-forward "openai-codex" nil t))
   (should (search-forward " / " nil t))
   (should (search-forward "gpt-5.5" nil t))
   (should (search-forward "Subagents: " nil t))
   (should (search-forward "openrouter" nil t))
   (should (search-forward "deepseek/deepseek-chat-v3.1" nil t))))

(ert-deftest zerostack-test-sends-all-protocol-commands ()
  (zerostack-test--with-buffer
   (let (sent)
     (setq zerostack--send-function (lambda (line) (push line sent)))
     (setq zerostack--cols 100)

     (zerostack-send-hello)
     (zerostack-attach)
     (zerostack-render)
     (zerostack-set-view 120)
     (zerostack-provider-menu "openai-codex")
     (zerostack-model-menu "gpt-5.5")
     (zerostack-subagent-provider-menu "openrouter")
     (zerostack-subagent-model-menu "deepseek/deepseek-chat-v3.1")
     (zerostack-goal)
     (zerostack-clear-goal)
     (zerostack-list-tools)
     (zerostack-mcp)
     (zerostack-thinking-menu "off")
     (zerostack-send-prompt "hello\nworld")
     (zerostack-compact)
     (zerostack-compact "keep recent tool output")
     (zerostack-loop-start "fix bugs" 2 "cargo test")
     (zerostack-loop-status)
     (zerostack-loop-stop)
     (zerostack-add-file "/tmp/photo.png")
     (zerostack-list-files)
     (cl-letf (((symbol-function 'yes-or-no-p) (lambda (&rest _) t)))
       (zerostack-drop-all-files))
     (zerostack-abort)
     (zerostack-permission-answer 42 'allow-always "bash cargo test")
     (zerostack-request-sessions 7)
     (zerostack-request-status)

     (let ((forms (zerostack-test--sent-forms sent)))
       (should (equal (mapcar #'car forms)
                      '(hello attach render set-view provider model subagent-provider subagent-model goal goal list-tools mcp thinking prompt compact compact loop-start
                              loop-status loop-stop file-add file-list file-drop-all abort
                              permission-answer list-sessions status)))
       (should (equal (nth 0 forms) '(hello :request 1 :protocol 1 :cols 100)))
       (should (equal (nth 1 forms) '(attach :request 2 :cols 100)))
       (should (equal (nth 2 forms) '(render :request 3 :cols 100)))
       (should (equal (nth 3 forms) '(set-view :request 4 :cols 120)))
       (should (equal (nth 4 forms) '(provider :request 5 :provider "openai-codex")))
       (should (equal (nth 5 forms) '(model :request 6 :model "gpt-5.5")))
       (should (equal (nth 6 forms) '(subagent-provider :request 7 :provider "openrouter")))
       (should (equal (nth 7 forms) '(subagent-model :request 8 :model "deepseek/deepseek-chat-v3.1")))
       (should (equal (nth 8 forms) '(goal :request 9 :action show)))
       (should (equal (nth 9 forms) '(goal :request 10 :action clear)))
       (should (equal (nth 10 forms) '(list-tools :request 11)))
       (should (equal (nth 11 forms) '(mcp :request 12)))
       (should (equal (nth 12 forms) '(thinking :request 13 :level "off")))
       (should (equal (nth 13 forms) '(prompt :request 14 :text "hello\nworld")))
       (should (equal (nth 14 forms) '(compact :request 15)))
       (should (equal (nth 15 forms)
                      '(compact :request 16 :instructions "keep recent tool output")))
       (should (equal (nth 16 forms)
                      '(loop-start :request 17 :prompt "fix bugs" :max 2
                                   :run "cargo test")))
       (should (equal (nth 17 forms) '(loop-status :request 18)))
       (should (equal (nth 18 forms) '(loop-stop :request 19)))
       (should (equal (nth 19 forms) '(file-add :request 20 :path "/tmp/photo.png")))
       (should (equal (nth 20 forms) '(file-list :request 21)))
       (should (equal (nth 21 forms) '(file-drop-all :request 22)))
       (should (equal (nth 22 forms) '(abort :request 23)))
       (should (equal (nth 23 forms)
                      '(permission-answer :request 42 :decision allow-always
                                          :pattern "bash cargo test")))
       (should (equal (nth 24 forms) '(list-sessions :request 24 :limit 7)))
       (should (equal (nth 25 forms) '(status :request 25)))))))

(ert-deftest zerostack-test-send-form-escapes-newlines ()
  (zerostack-test--with-buffer
   (let ((line (zerostack--send-form '(prompt :request 1 :text "hello\nworld"))))
     (should (equal (length (split-string line "\n")) 2))
     (should (equal (zerostack-test--decode-line line)
                    '(prompt :request 1 :text "hello\nworld"))))))

(ert-deftest zerostack-test-compact-marks-buffer-busy-until-complete ()
  (zerostack-test--with-buffer
   (let (sent notified)
     (setq zerostack--send-function (lambda (line) (push line sent)))
     (cl-letf (((symbol-function 'zerostack--notify-ready)
                (lambda () (setq notified t))))
       (zerostack-compact)
       (should zerostack--thinking)
       (should (equal zerostack--status "compacting..."))
       (should (equal (car (zerostack-test--sent-forms sent))
                      '(compact :request 1)))
       (zerostack--handle-ok '(:request 1 :compacted t :messages 2 :saved-tokens 10 :message "compressed"))
       (should-not zerostack--thinking)
       (should-not zerostack--status)
       (should-not notified)
       (sit-for 0.25)
       (should notified)))))

(ert-deftest zerostack-test-compact-error-clears-busy-state ()
  (zerostack-test--with-buffer
   (zerostack--set-thinking t)
   (zerostack--set-status "compacting...")
   (zerostack--handle-form '(error :request 1 :message "cannot compact while an agent turn is running"))
   (should-not zerostack--thinking)
   (should-not zerostack--status)
   (should (string-match-p "cannot compact" (buffer-string)))))

(ert-deftest zerostack-test-compact-ok-requests-render-refresh ()
  (zerostack-test--with-buffer
   (let (sent)
     (setq zerostack--send-function (lambda (line) (push line sent)))
     (zerostack--handle-ok '(:request 1 :compacted t :messages 2 :saved-tokens 10 :message "compressed"))
     (should (equal (car (zerostack-test--sent-forms sent))
                    '(render :request 1 :cols 100))))))

(ert-deftest zerostack-test-compact-done-requests-render-refresh ()
  (zerostack-test--with-buffer
   (let (sent)
     (setq zerostack--send-function (lambda (line) (push line sent)))
     (zerostack--handle-event '(:type compact-done :compacted t))
     (should (equal (car (zerostack-test--sent-forms sent))
                    '(render :request 1 :cols 100))))))

(ert-deftest zerostack-test-goal-nudge-cancels-ready-notification ()
  (zerostack-test--with-buffer
   (let (notified)
     (cl-letf (((symbol-function 'zerostack--notify-ready)
                (lambda () (setq notified t))))
       (zerostack--set-thinking t)
       (zerostack--handle-form
        '(event :seq 1 :session "s" :type done :turn 1))
       (should-not zerostack--thinking)
       (zerostack--handle-form
        '(event :seq 2 :session "s" :type goal-nudge :message "goal still open; continuing..."))
       (should zerostack--thinking)
       (sit-for 0.25)
       (should-not notified)))))

(ert-deftest zerostack-test-compact-finished-clears-busy-state ()
  (zerostack-test--with-buffer
   (zerostack--handle-event '(:type compact-started))
   (should zerostack--thinking)
   (should (equal zerostack--status "compacting..."))
   (zerostack--handle-event '(:type compact-finished))
   (should-not zerostack--thinking)
   (should-not zerostack--status)))

(ert-deftest zerostack-test-mid-turn-compact-done-keeps-buffer-active ()
  (zerostack-test--with-buffer
   (zerostack--handle-event '(:type compact-started :mid-turn t))
   (zerostack--handle-event '(:type compact-done :mid-turn t))
   (should zerostack--thinking)
   (should (equal zerostack--status "continuing..."))))

(ert-deftest zerostack-test-stream-output-restores-thinking-after-stale-idle ()
  (zerostack-test--with-buffer
   (zerostack--set-thinking nil)
   (zerostack--set-status nil)
   (zerostack--handle-event
    '(:type assistant-render :replace-from 0
            :lines ((:text "still running" :face zs-normal))))
   (should zerostack--thinking)
   (should (equal zerostack--status "thinking..."))))

(ert-deftest zerostack-test-completion-call-restores-thinking-after-stale-idle ()
  (zerostack-test--with-buffer
   (zerostack--set-thinking nil)
   (zerostack--handle-event '(:type completion-call :provider "openai" :model "gpt"))
   (should zerostack--thinking)))

(ert-deftest zerostack-test-command-menu-fallback-dispatches-to-protocol ()
  (zerostack-test--with-buffer
   (let (dispatched)
     (let ((choices '("attach" "compact" "loop" "thinking" "provider" "model" "subagent-provider" "subagent-model" "goal" "clear-goal" "tools" "mcp" "view" "restart")))
       (cl-letf (((symbol-function 'completing-read)
                  (lambda (&rest _) (pop choices)))
                 ((symbol-function 'zerostack-attachment-menu)
                  (lambda () (interactive) (push 'attach dispatched)))
                 ((symbol-function 'zerostack-compact)
                  (lambda () (interactive) (push 'compact dispatched)))
                 ((symbol-function 'zerostack-loop)
                  (lambda () (interactive) (push 'loop dispatched)))
                 ((symbol-function 'zerostack-thinking-menu)
                  (lambda (&optional _) (interactive) (push 'thinking dispatched)))
                 ((symbol-function 'zerostack-provider-menu)
                  (lambda (&optional _) (interactive) (push 'provider dispatched)))
                 ((symbol-function 'zerostack-model-menu)
                  (lambda (&optional _) (interactive) (push 'model dispatched)))
                 ((symbol-function 'zerostack-subagent-provider-menu)
                  (lambda (&optional _) (interactive) (push 'subagent-provider dispatched)))
                 ((symbol-function 'zerostack-subagent-model-menu)
                  (lambda (&optional _) (interactive) (push 'subagent-model dispatched)))
                  ((symbol-function 'zerostack-goal)
                   (lambda (&optional _) (interactive) (push 'goal dispatched)))
                  ((symbol-function 'zerostack-clear-goal)
                   (lambda () (interactive) (push 'clear-goal dispatched)))
                  ((symbol-function 'zerostack-list-tools)
                   (lambda () (interactive) (push 'tools dispatched)))
                  ((symbol-function 'zerostack-mcp)
                   (lambda () (interactive) (push 'mcp dispatched)))
                 ((symbol-function 'zerostack-set-view)
                  (lambda () (interactive) (push 'view dispatched)))
                 ((symbol-function 'zerostack-restart-daemon)
                  (lambda () (interactive) (push 'restart dispatched))))
         (dotimes (_ 14)
           (zerostack--command-menu-fallback))))
     (should (equal (nreverse dispatched) '(attach compact loop thinking provider model subagent-provider subagent-model goal clear-goal tools mcp view restart))))))

(ert-deftest zerostack-test-restart-daemon-reuses_session_without_closing_buffer ()
  (zerostack-test--with-buffer
   (let (deleted started)
     (setq zerostack--session "session-1"
           zerostack--socket "/tmp/old.sock"
           zerostack--line-buffer "partial")
     (cl-letf (((symbol-function 'zerostack--delete-current-processes)
                (lambda () (setq deleted t)))
               ((symbol-function 'zerostack--start-server)
                (lambda (args) (setq started args))))
       (zerostack-restart-daemon)
       (should deleted)
       (should (equal started '("--session" "session-1")))
       (should-not zerostack--socket)
       (should (string-empty-p zerostack--line-buffer))
       (should (string-match-p "restarting zerostack --emacs" (buffer-string)))))))

(ert-deftest zerostack-test-command-menu-permission-selection ()
  (zerostack-test--with-buffer
   (let (sent)
     (setq zerostack--send-function (lambda (line) (push line sent)))
     (puthash 9 '(:request 9 :tool "bash") zerostack--pending-permissions)
     (let ((choices '("9  bash" "allow-always")))
       (cl-letf (((symbol-function 'completing-read)
                  (lambda (&rest _) (pop choices)))
                 ((symbol-function 'read-string)
                  (lambda (&rest _) "bash .*cargo")))
         (zerostack-permission-menu)))
     (should (equal (car (zerostack-test--sent-forms sent))
                    '(permission-answer :request 9 :decision allow-always
                                        :pattern "bash .*cargo"))))))

(ert-deftest zerostack-test-command-menu-skill-selection-inserts-directive ()
  (zerostack-test--with-buffer
   (cl-letf (((symbol-function 'zerostack--discover-skills)
              (lambda ()
                '((:name "render-review"
			 :description "Review rendered output"
			 :path "/repo/.claude/skills/render-review/SKILL.md"))))
             ((symbol-function 'completing-read)
              (lambda (&rest _) "render-review — Review rendered output")))
     (zerostack-skill-menu)
     (should (string-match-p
              "Use the render-review skill at /repo/.claude/skills/render-review/SKILL.md\."
              (buffer-substring-no-properties
               (marker-position zerostack--input-marker)
               (point-max)))))))

(ert-deftest zerostack-test-attachment-menu-adds_path_and_clipboard_file ()
  (let* ((dir (make-temp-file "zerostack-attach" t))
         (file (expand-file-name "image.png" dir)))
    (unwind-protect
        (progn
          (with-temp-file file (insert "png"))
          (zerostack-test--with-buffer
           (let (sent)
             (setq zerostack--send-function (lambda (line) (push line sent)))
             (zerostack-add-file file)
             (cl-letf (((symbol-function 'gui-get-selection)
                        (lambda (&rest _) file)))
               (zerostack-add-clipboard))
             (should (equal (zerostack-test--sent-forms sent)
                            `((file-add :request 1 :path ,file)
                              (file-add :request 2 :path ,file)))))))
      (delete-directory dir t))))

(ert-deftest zerostack-test-clipboard_uri_list_adds_copied_file ()
  (let* ((dir (make-temp-file "zerostack-uri-list" t))
         (file (expand-file-name "copied file.txt" dir)))
    (unwind-protect
        (progn
          (with-temp-file file (insert "copied"))
          (zerostack-test--with-buffer
           (let (sent)
             (setq zerostack--send-function (lambda (line) (push line sent)))
             (cl-letf (((symbol-function 'gui-get-selection)
                        (lambda (_selection target)
                          (when (eq target (intern "x-special/gnome-copied-files"))
                            (concat "copy\nfile://"
                                    (replace-regexp-in-string " " "%20" file))))))
               (zerostack-add-clipboard))
             (should (equal (zerostack-test--sent-forms sent)
                            `((file-add :request 1 :path ,file)))))))
      (delete-directory dir t))))

(ert-deftest zerostack-test-clipboard_command_uri_list_fallback ()
  (let* ((dir (make-temp-file "zerostack-uri-command" t))
         (file (expand-file-name "command.txt" dir)))
    (unwind-protect
        (progn
          (with-temp-file file (insert "copied"))
          (zerostack-test--with-buffer
           (let (sent)
             (setq zerostack--send-function (lambda (line) (push line sent)))
             (cl-letf (((symbol-function 'gui-get-selection)
                        (lambda (&rest _) nil))
                       ((symbol-function 'zerostack--clipboard-command-output)
                        (lambda (_binary program &rest args)
                          (when (and (equal program "wl-paste")
                                     (member "text/uri-list" args))
                            (concat "file://" file)))))
               (zerostack-add-clipboard))
             (should (equal (zerostack-test--sent-forms sent)
                            `((file-add :request 1 :path ,file)))))))
      (delete-directory dir t))))

(ert-deftest zerostack-test-clipboard_image_is_written_to_temp_file ()
  (zerostack-test--with-buffer
   (let (sent)
     (setq zerostack--send-function (lambda (line) (push line sent)))
     (cl-letf (((symbol-function 'gui-get-selection)
                (lambda (_selection target)
                  (when (eq target 'image/png) "PNGDATA"))))
       (zerostack-add-clipboard))
     (let* ((form (car (zerostack-test--sent-forms sent)))
            (path (plist-get (cdr form) :path)))
       (should (equal (car form) 'file-add))
       (should (string-suffix-p ".png" path))
       (should (file-exists-p path))
       (should (member path zerostack--clipboard-temp-files))
       (zerostack--cleanup-clipboard-temp-files)
       (should-not (file-exists-p path))))))

(ert-deftest zerostack-test-yank_media_image_is_attached ()
  (zerostack-test--with-buffer
   (let (sent)
     (setq zerostack--send-function (lambda (line) (push line sent)))
     (zerostack--yank-media-image 'image/png "PNGDATA")
     (let* ((form (car (zerostack-test--sent-forms sent)))
            (path (plist-get (cdr form) :path)))
       (should (equal (car form) 'file-add))
       (should (string-suffix-p ".png" path))
       (should (file-exists-p path))
       (should (member path zerostack--clipboard-temp-files))
       (zerostack--cleanup-clipboard-temp-files)
       (should-not (file-exists-p path))))))

(ert-deftest zerostack-test-yank-attaches-image-before-regular-yank ()
  (zerostack-test--with-buffer
   (let (sent yanked)
     (setq zerostack--send-function (lambda (line) (push line sent)))
     (cl-letf (((symbol-function 'gui-get-selection)
                (lambda (_selection target)
                  (when (eq target 'image/png) "PNGDATA")))
               ((symbol-function 'zerostack--clipboard-command-output)
                (lambda (&rest _) nil))
               ((symbol-function 'yank)
                (lambda (&rest _)
                  (setq yanked t))))
       (zerostack-yank))
     (let* ((form (car (zerostack-test--sent-forms sent)))
            (path (plist-get (cdr form) :path)))
       (should-not yanked)
       (should (equal (car form) 'file-add))
       (should (string-suffix-p ".png" path))
       (zerostack--cleanup-clipboard-temp-files)))))

(ert-deftest zerostack-test-yank-falls-back-to-regular-yank ()
  (zerostack-test--with-buffer
   (let (yanked)
     (cl-letf (((symbol-function 'zerostack-add-clipboard)
                (lambda (&optional quiet)
                  (should quiet)
                  nil))
               ((symbol-function 'yank)
                (lambda (&rest _)
                  (interactive)
                  (setq yanked t))))
       (zerostack-yank))
     (should yanked)
     (should (eq (lookup-key zerostack-mode-map [remap yank])
                 'zerostack-yank)))))

(ert-deftest zerostack-test-skill-menu-falls-back-to-board-metadata ()
  (let* ((root (make-temp-file "zerostack-skills" t))
         (stale (expand-file-name "stale" root))
         (worktree (expand-file-name "demo-worktree" root))
         (skill-file (expand-file-name ".claude/skills/render-review/SKILL.md" worktree))
         (snapshot `(zerostack-board
                     :version 1
                     :projects
                     ((:name "demo"
			     :path ,worktree
			     :worktrees
			     ((:path ,worktree
				     :sessions
				     ((:id "session-1"
					   :title "Demo session"
					   :cwd ,worktree
					   :socket "/tmp/demo.sock")))))))))
    (unwind-protect
        (progn
          (make-directory (file-name-directory skill-file) t)
          (with-temp-file skill-file
            (insert "---\n"
                    "name: render-review\n"
                    "description: Review demo rendering.\n"
                    "---\n"
                    "Use for the native Emacs demo.\n"))
          (make-directory stale t)
          (zerostack-test--with-buffer
           (let ((zerostack--home-skill-dirs nil)
                 (zerostack--worktree-dir stale)
                 (zerostack--cwd stale)
                 (zerostack--session "session-1")
                 (zerostack--socket "/tmp/demo.sock"))
             (cl-letf (((symbol-function 'zerostack-board--fetch)
                        (lambda () snapshot))
                       ((symbol-function 'completing-read)
                        (lambda (&rest _) "render-review — Review demo rendering.")))
               (zerostack-skill-menu))
             (should (equal zerostack--worktree-dir
                            (file-name-as-directory worktree)))
             (should (string-match-p
                      (regexp-quote
                       (format "Use the render-review skill at %s. " skill-file))
                      (buffer-substring-no-properties
                       (marker-position zerostack--input-marker)
                       (point-max)))))))
      (delete-directory root t))))

(ert-deftest zerostack-test-input-line-is-erc-style ()
  (zerostack-test--with-buffer
   (let (sent)
     (setq zerostack--send-function (lambda (line) (push line sent)))
     (goto-char (point-max))
     (insert "normal prompt")
     (zerostack-send-input)
     (should (equal (car (zerostack-test--sent-forms sent))
                    '(prompt :request 1 :text "normal prompt")))
     (should (string-empty-p
              (buffer-substring-no-properties
               (marker-position zerostack--input-marker)
               (point-max))))

     (goto-char (point-max))
     (insert "/compact now")
     (zerostack-send-input)
     (let ((forms (zerostack-test--sent-forms sent)))
       (should (equal (cadr forms)
                      '(prompt :request 2 :text "/compact now")))))))

(ert-deftest zerostack-test-input-line-shows-thinking-level ()
  (zerostack-test--with-buffer
   (setq zerostack--model "gpt-test")
   (zerostack--handle-ok '(:request 1 :thinking "off" :reasoning-tokens 12000))
   (should (string-match-p "thinking:off | thinking:12k | gpt-test" (buffer-string)))))

(ert-deftest zerostack-test-control-return-inserts-newline ()
  (zerostack-test--with-buffer
   (goto-char (point-max))
   (insert "hello")
   (zerostack-insert-newline)
   (insert "world")
   (should (equal (buffer-substring-no-properties
                   (marker-position zerostack--input-marker)
                   (marker-position zerostack--controls-start-marker))
                  "hello\nworld"))))

(ert-deftest zerostack-test-only-user-input-records-undo ()
  (zerostack-test--with-buffer
   (setq buffer-undo-list nil)
   (zerostack--replace-lines 0 '((:text "server" :face zs-normal)))
   (zerostack--set-status "thinking")
   (zerostack--set-notice "notice")
   (puthash 1 '(:request 1 :tool "bash" :input "echo hi") zerostack--pending-permissions)
   (zerostack--refresh-permission-buttons)
   (clrhash zerostack--pending-permissions)
   (zerostack--refresh-permission-buttons)
   (zerostack--insert-input "generated")
   (zerostack--clear-input)
   (should (null buffer-undo-list))
   (goto-char (marker-position zerostack--controls-start-marker))
   (insert "typed")
   (should buffer-undo-list)
   (let ((inhibit-read-only t))
     (primitive-undo 1 buffer-undo-list))
   (should (string-empty-p
            (buffer-substring-no-properties
             (marker-position zerostack--input-marker)
             (marker-position zerostack--controls-start-marker))))))

(ert-deftest zerostack-test-buffered-protocol-input ()
  (zerostack-test--with-buffer
   (zerostack--consume-chunk "(ready :protocol 1 :session \"abc\"")
   (should (equal zerostack--line-buffer "(ready :protocol 1 :session \"abc\""))
   (zerostack--consume-chunk " :pid 1 :socket \"/tmp/sock\")\n(event :seq 1 :session \"abc\" :type session-render ")
   (should (equal zerostack--session "abc"))
   (zerostack--consume-chunk ":replace-from 0 :lines ((:text \"hello\" :face zs-normal)))\n")
   (should (string-empty-p zerostack--line-buffer))
   (should (string-match-p "hello" (buffer-string)))
   (should (= 1 (length zerostack--line-markers)))))

(ert-deftest zerostack-test-handles-every-server-form-and-event ()
  (zerostack-test--with-buffer
   (zerostack--handle-form
    '(ready :protocol 1 :session "s" :pid 123 :socket "/tmp/sock"))
   (should (equal zerostack--session "s"))
   (zerostack--handle-form
    '(ok :request 1 :protocol 1 :session "s" :pid 123 :cols 111 :socket "/tmp/sock"
         :provider "openai-codex" :model "gpt-5.5" :tokens 20 :context-window 100))
   (should (= zerostack--cols 111))
   (should (equal zerostack--provider "openai-codex"))
   (should (equal zerostack--model "gpt-5.5"))
   (should (equal zerostack--tokens 20))
   (should (equal zerostack--context-window 100))
   (should (string-match-p "gpt-5.5 | (20/20%)" (buffer-string)))
   (zerostack--handle-form '(error :request 2 :message "bad request"))
   (should (string-match-p "bad request" zerostack--last-notice))
   (zerostack--handle-form
    '(sessions :request 3
               :items ((:session "s1" :pid 1 :cwd "/repo" :model "m"
				 :provider "p" :created-at "c" :updated-at "u"
				 :title "t" :tokens 30 :context-window 100 :protocol 1 :socket "/tmp/s1"))))
   (should (string-match-p "sessions" (buffer-string)))
   (zerostack--handle-form
    '(status :request 4
             :session (:session "s" :pid 123 :cwd "/repo" :model "m"
				:provider "p" :created-at "c" :updated-at "u"
				:title "t" :protocol 1 :socket "/tmp/sock")))
   (should (equal zerostack--provider "p"))
   (should (equal zerostack--model "m"))
   (zerostack--handle-form
    '(event :seq 0 :session "s" :type loop-started :turn 1
            :active t :iteration 1 :label "LOOP 1/2" :max 2
            :plan "LOOP_PLAN.md" :prompt "fix bugs"))
   (should zerostack--loop-active)
   (should zerostack--thinking)
   (should (equal zerostack--loop-label "LOOP 1/2"))
   (should (string-match-p "loop LOOP 1/2" (buffer-string)))

   (zerostack--handle-form
    '(event :seq 1 :session "s" :type session-render :replace-from 0
            :lines ((:text "session" :face zs-heading))))
   (should (= 1 (length zerostack--line-markers)))
   (zerostack--handle-form
    '(event :seq 2 :session "s" :type user-render :turn 1 :replace-from 1
            :lines ((:text "> hi" :face zs-user))))
   (zerostack--handle-form
    `(event :seq 3 :session "s" :type assistant-render :turn 1 :replace-from 2
            :lines ((:text "< math $x$" :face zs-normal
			   :latex (,zerostack-test--latex-item)))))
   (should (gethash "turn-1-latex-1" zerostack--latex-items))
   (zerostack--handle-form
    `(event :seq 4 :session "s" :type reasoning-render :turn 1 :replace-from 3
            :lines ((:text "thinking: 17 B" :face zs-reasoning
			   :artifact ,zerostack-test--tool-artifact))))
   (save-excursion
     (goto-char (point-min))
     (search-forward "thinking: 17 B")
     (backward-char 1)
     (should (equal (plist-get (get-text-property (point) 'zerostack-artifact) :path)
                    "/tmp/zerostack-tool.txt"))
     (let ((face (get-text-property (point) 'face)))
       (should (memq 'zerostack-link-face (if (listp face) face (list face))))
       (should (memq 'zerostack-reasoning-face (if (listp face) face (list face))))))
   (zerostack--handle-form
    '(event :seq 5 :session "s" :type tool-call :turn 1
            :name "bash" :summary "bash cargo test" :args "{}"))
   (zerostack--handle-form
    '(event :seq 6 :session "s" :type subagent-tool-call :turn 1
            :name "task" :summary "task explore" :args "{}"))
   (zerostack--handle-form
    `(event :seq 7 :session "s" :type tool-result :turn 1 :name "bash"
            :chars 17 :preview "ok" :artifact ,zerostack-test--tool-artifact))
   (should (cl-find "/tmp/zerostack-tool.txt" zerostack--artifacts
                    :key (lambda (artifact) (plist-get artifact :path))
                    :test #'equal))
   (zerostack--handle-form
    '(event :seq 8 :session "s" :type completion-call :turn 1
            :call-index 0 :input-tokens 10 :output-tokens 2))
   (zerostack--handle-form
    '(event :seq 9 :session "s" :type permission-request :request 8
            :tool "bash" :input "cargo test" :suggested-pattern "cargo test"))
   (should (gethash 8 zerostack--pending-permissions))
   (zerostack--handle-form
    '(event :seq 10 :session "s" :type permission-answered :request 8
            :decision allow-once))
   (zerostack--handle-form
    '(event :seq 11 :session "s" :type reasoning :turn 1 :preview "thinking"
            :artifact nil))
   (zerostack--handle-form
    '(event :seq 12 :session "s" :type done :turn 1
            :input-tokens 10 :output-tokens 2))
   (should zerostack--loop-active)
   (setq zerostack--loop-active nil)
   (zerostack--handle-form
    '(event :seq 12 :session "s" :type done :turn 1
            :input-tokens 10 :output-tokens 2))
   (should-not zerostack--thinking)
   (zerostack--handle-form
    '(event :seq 12 :session "s" :type goal-nudge :message "goal still open; continuing..."))
   (should zerostack--thinking)
   (should (equal zerostack--status "continuing goal"))
   (zerostack--handle-form
    `(event :seq 13 :session "s" :type latex-preview-ready :turn 1
            :items (,zerostack-test--latex-item)))
   (should (cl-some (lambda (overlay) (overlay-get overlay 'zerostack-latex))
                    (overlays-in (point-min) (point-max))))
   (zerostack--handle-form
    '(event :seq 14 :session "s" :type loop-stopped :reason max))
   (should-not zerostack--loop-active)
   (zerostack--handle-form '(event :seq 15 :session "s" :type aborted))
   (zerostack--handle-form
    '(event :seq 16 :session "s" :type error :turn 1 :message "failed"))
   (should (string-match-p "failed" zerostack--last-notice))))

(ert-deftest zerostack-test-render-replace-from-deletes-tail-only ()
  (zerostack-test--with-buffer
   (zerostack--replace-lines
    0
    '((:text "a" :face zs-normal)
      (:text "b" :face zs-normal)
      (:text "c" :face zs-normal)))
   (should (= 3 (length zerostack--line-markers)))
   (zerostack--replace-lines
    1
    '((:text "B" :face zs-heading)))
   (should (= 2 (length zerostack--line-markers)))
   (let ((text (buffer-string)))
     (should (string-match-p "a" text))
     (should (string-match-p "B" text))
     (should-not (string-match-p "c" text)))))

(ert-deftest zerostack-test-local-notices-do-not-affect-render-indexes ()
  (zerostack-test--with-buffer
   (zerostack--replace-lines
    0
    '((:text "transcript a" :face zs-normal)))
   (zerostack--append-local-line "thinking locally" 'zs-reasoning)
   (should (= 1 (length zerostack--line-markers)))
   (zerostack--replace-lines
    1
    '((:text "transcript b" :face zs-normal)))
   (should (= 2 (length zerostack--line-markers)))
   (let* ((text (buffer-string))
          (a (string-match "transcript a" text))
          (b (string-match "transcript b" text))
          (notice (string-match "thinking locally" text)))
     (should a)
     (should b)
     (should notice)
     (should (< a b))
     (should (< b notice)))))

(ert-deftest zerostack-test-wire-line-applies-span-faces ()
  (zerostack-test--with-buffer
   (zerostack--replace-lines
    0
    '((:text "Title bold code table"
	     :face zs-normal
	     :spans ((:text "Title" :face zs-heading)
		     (:text " bold" :face zs-bold)
		     (:text " code" :face zs-code)
		     (:text " table" :face zs-table)))))
   (let ((title (text-property-any (point-min) (point-max) 'face 'zerostack-heading-face))
         (bold (text-property-any (point-min) (point-max) 'face 'zerostack-bold-face))
         (code (text-property-any (point-min) (point-max) 'face 'zerostack-code-face))
         (table (text-property-any (point-min) (point-max) 'face 'zerostack-table-face)))
     (should title)
     (should bold)
     (should code)
     (should table))))

(ert-deftest zerostack-test-routine-events-do-not-insert-local-lines ()
  (zerostack-test--with-buffer
   (zerostack--replace-lines
    0
    '((:text "server transcript" :face zs-normal)))
   (zerostack--handle-form
    '(ready :protocol 1 :session "s" :pid 123 :socket "/tmp/sock"))
   (zerostack--handle-form
    `(event :seq 1 :session "s" :type tool-result :turn 1 :name "bash"
            :chars 17 :preview "ok" :artifact ,zerostack-test--tool-artifact))
   (zerostack--handle-form
    '(event :seq 2 :session "s" :type done :turn 1
            :input-tokens 10 :output-tokens 2))
   (let ((text (buffer-string)))
     (should (string-match-p "server transcript" text))
     (should-not (string-match-p "ready:" text))
     (should-not (string-match-p "tool output:" text))
     (should-not (string-match-p "done:" text)))
   (zerostack--handle-form
    '(event :seq 3 :session "s" :type tool-render :turn 1 :replace-from 1
            :lines ((:text "tool rendered by server" :face zs-tool))))
   (should (string-match-p "tool rendered by server" (buffer-string)))))

(ert-deftest zerostack-test-render-updates-preserve-current-input ()
  (zerostack-test--with-buffer
   (zerostack--replace-lines
    0
    '((:text "latest message" :face zs-normal)))
   (goto-char (point-max))
   (insert "draft input")
   (zerostack--replace-lines
    1
    '((:text "stream update" :face zs-normal)))
   (should (equal (buffer-substring-no-properties
                   (marker-position zerostack--input-marker)
                   (point-max))
                  "draft input"))
   (let ((text (buffer-string)))
     (should (string-match-p "latest message" text))
     (should (string-match-p "stream update" text)))))

(ert-deftest zerostack-test-render-updates-preserve-input-point ()
  (zerostack-test--with-buffer
   (zerostack--replace-lines
    0
    '((:text "latest message" :face zs-normal)))
   (goto-char (point-max))
   (insert "draft input")
   (goto-char (+ (marker-position zerostack--input-marker) 5))
   (zerostack--replace-lines
    1
    '((:text "stream update" :face zs-normal)))
   (should (= (point) (+ (marker-position zerostack--input-marker) 5)))
   (should (equal (buffer-substring-no-properties
                   (marker-position zerostack--input-marker)
                   (point-max))
                  "draft input"))))

(ert-deftest zerostack-test-render-updates-preserve-transcript-point ()
  (zerostack-test--with-buffer
   (zerostack--replace-lines
    0
    '((:text "first transcript" :face zs-normal)
      (:text "second transcript" :face zs-normal)))
   (goto-char (point-min))
   (search-forward "first")
   (let ((expected (point)))
     (zerostack--replace-lines
      2
      '((:text "third transcript" :face zs-normal)))
     (should (= (point) expected))
     (should (looking-back "first" (line-beginning-position))))))

(ert-deftest zerostack-test-status-refresh-preserves-transcript-point ()
  (zerostack-test--with-buffer
   (zerostack--replace-lines
    0
    '((:text "transcript line" :face zs-normal)))
   (goto-char (point-min))
   (search-forward "transcript")
   (let ((expected (point)))
     (zerostack--set-status "permission #8 bash")
     (should (= (point) expected))
     (should (looking-back "transcript" (line-beginning-position))))))

(ert-deftest zerostack-test-transient-notice-expires-without-clearing-status ()
  (zerostack-test--with-buffer
   (let ((zerostack-notice-timeout 0.05))
     (zerostack--set-status "permission #8 bash")
     (zerostack--set-notice "aborted")
     (let ((text (buffer-string)))
       (should (string-match-p "aborted" text))
       (should (string-match-p "permission #8 bash" text)))
     (zerostack-test--wait-until (lambda () (null zerostack--notice)) 1.0)
     (let ((text (buffer-string)))
       (should-not (string-match-p "aborted" text))
       (should (string-match-p "permission #8 bash" text))))))

(ert-deftest zerostack-test-prompt-indicates-thinking-until-done ()
  (zerostack-test--with-buffer
   (let (sent)
     (setq zerostack--send-function (lambda (line) (push line sent)))
     (zerostack-send-prompt "hello")
     (should zerostack--thinking)
     (should (string-match-p "zs thinking> " (buffer-string)))
     (zerostack--handle-form
      '(event :seq 1 :session "s" :type done :turn 1
              :input-tokens 1 :output-tokens 1))
     (should-not zerostack--thinking)
     (should (string-match-p "zs> " (buffer-string))))))

(ert-deftest zerostack-test-ready-notification-on-thinking-transition ()
  (zerostack-test--with-buffer
   (let ((zerostack-notify-on-ready t)
         processes)
     (setq zerostack--session-title "Demo")
     (cl-letf (((symbol-function 'executable-find)
                (lambda (program) (and (equal program "notify-send") program)))
               ((symbol-function 'start-process)
                (lambda (&rest args) (push args processes))))
       (zerostack--set-thinking t)
       (zerostack--set-thinking nil)
       (should-not processes)
       (sit-for 0.25)
       (should (equal processes
                      '(("zerostack-notify" nil "notify-send" "zerostack" "Demo needs input: ready"))))))))

(ert-deftest zerostack-test-loop-state-updates-prompt-and_stop_command ()
  (zerostack-test--with-buffer
   (let (sent)
     (setq zerostack--send-function (lambda (line) (push line sent)))
     (zerostack-loop-start "fix bugs" 2 "cargo test")
     (should zerostack--thinking)
     (should (equal (car (zerostack-test--sent-forms sent))
                    '(loop-start :request 1 :prompt "fix bugs" :max 2
                                 :run "cargo test")))
     (zerostack--handle-form
      '(event :seq 1 :session "s" :type loop-iteration :turn 1
              :active t :iteration 1 :label "LOOP 1/2" :max 2
              :plan "LOOP_PLAN.md" :prompt "fix bugs"))
     (should zerostack--loop-active)
     (should (string-match-p "loop LOOP 1/2" (buffer-string)))
     (should (string-match-p "zs loop thinking> " (buffer-string)))
     (zerostack--handle-form
      '(event :seq 2 :session "s" :type done :turn 1
              :input-tokens 1 :output-tokens 1))
     (should zerostack--loop-active)
     (should-not zerostack--thinking)
     (should (string-match-p "zs loop> " (buffer-string)))
     (zerostack-loop-stop)
     (let ((forms (zerostack-test--sent-forms sent)))
       (should (equal (cadr forms) '(loop-stop :request 2))))
     (zerostack--handle-form
      '(event :seq 3 :session "s" :type loop-stopped :reason stopped))
     (should-not zerostack--loop-active)
     (should-not zerostack--thinking)
     (should (string-match-p "loop stopped" (buffer-string))))))

(ert-deftest zerostack-test-permission-request-notifies-needs-input ()
  (zerostack-test--with-buffer
   (let ((zerostack-notify-on-ready t)
         processes)
     (setq zerostack--session-title "Demo")
     (cl-letf (((symbol-function 'executable-find)
                (lambda (program) (and (equal program "notify-send") program)))
               ((symbol-function 'start-process)
                (lambda (&rest args) (push args processes))))
       (zerostack--handle-form
        '(event :seq 1 :session "s" :type permission-request
                :request 8 :tool "bash" :input "pwd"))
       (should (equal processes
                      '(("zerostack-notify" nil "notify-send" "zerostack" "Demo needs input: permission #8 bash"))))))))

(ert-deftest zerostack-test-board-colors-open-permission-session-as-input-needed ()
  (let ((chat (generate-new-buffer " *zerostack-open-permission-session*")))
    (unwind-protect
        (progn
          (with-current-buffer chat
            (zerostack-mode)
            (setq zerostack--session "live-session"
                  zerostack--thinking t)
            (zerostack--handle-form
             '(event :seq 1 :session "live-session" :type permission-request
                     :request 8 :tool "bash" :input "pwd")))
          (zerostack-test--with-board-buffer
           (zerostack-test--expand-project "/repo/live")
           (zerostack-board--render zerostack-test--board-snapshot)
           (goto-char (point-min))
           (search-forward "live-wt/")
           (should (eq (get-text-property (1- (point)) 'face)
                       'zerostack-board-input-face))))
      (when (buffer-live-p chat)
        (kill-buffer chat)))))

(ert-deftest zerostack-test-prompt-indicates-waiting-for-permission ()
  (zerostack-test--with-buffer
   (let (sent)
     (setq zerostack--send-function (lambda (line) (push line sent)))
     (zerostack-send-prompt "hello")
     (should zerostack--thinking)
     (should (string-match-p "zs thinking> " (buffer-string)))
     (zerostack--handle-form
      '(event :seq 1 :session "s" :type permission-request
              :request 8 :tool "bash" :input "pwd"))
     (should (zerostack--pending-permissions-p))
     (should (string-match-p "permission #8 bash" (buffer-string)))
     (should-not (string-match-p "C-c C-m p to answer" (buffer-string)))
     (should (string-match-p "zs waiting for permission> " (buffer-string)))
     (should (string-match-p "permission: #8 bash" (buffer-string)))
     (should (string-match-p "allow once" (buffer-string)))
     (should (string-match-p "allow always" (buffer-string)))
     (should (string-match-p "deny" (buffer-string)))
     (zerostack--handle-form
      '(event :seq 2 :session "s" :type permission-answered
              :request 8 :decision allow-once))
     (should-not (zerostack--pending-permissions-p))
     (should zerostack--thinking)
     (should (string-match-p "zs thinking> " (buffer-string)))
     (should-not (string-match-p "waiting for permission" (buffer-string)))
     (zerostack--handle-form
      '(event :seq 3 :session "s" :type done :turn 1
              :input-tokens 1 :output-tokens 1))
     (should-not zerostack--thinking)
     (should-not (zerostack--pending-permissions-p))
     (should (string-match-p "zs> " (buffer-string))))))

(ert-deftest zerostack-test-permission-buttons-answer-and-input-excludes-row ()
  (zerostack-test--with-buffer
   (let (sent)
     (setq zerostack--send-function (lambda (line) (push line sent)))
     (zerostack--handle-form
      '(event :seq 1 :session "s" :type permission-request
              :request 8 :tool "bash" :input "pwd"
              :suggested-pattern "bash pwd"))
     (zerostack--insert-input "next prompt")
     (zerostack-send-input)
     (let ((forms (zerostack-test--sent-forms sent)))
       (should (equal (car forms) '(prompt :request 1 :text "next prompt"))))
     (goto-char (point-min))
     (search-forward "allow always")
     (let ((button (button-at (1- (point)))))
       (should button)
       (cl-letf (((symbol-function 'read-string)
                  (lambda (&rest _) "bash pwd")))
         (push-button button)))
     (let ((forms (zerostack-test--sent-forms sent)))
       (should (equal (cadr forms)
                      '(permission-answer :request 8 :decision allow-always
                                          :pattern "bash pwd")))))))

(ert-deftest zerostack-test-abort-clears-pending-permission-state ()
  (zerostack-test--with-buffer
   (let (sent)
     (setq zerostack--send-function (lambda (line) (push line sent)))
     (zerostack-send-prompt "hello")
     (zerostack--handle-form
      '(event :seq 1 :session "s" :type permission-request
              :request 8 :tool "bash" :input "pwd"))
     (should (zerostack--pending-permissions-p))
     (zerostack-abort)
     (should-not (zerostack--pending-permissions-p))
     (should-not zerostack--thinking)
     (should-not (string-match-p "permission #8" (buffer-string)))
     (let ((forms (zerostack-test--sent-forms sent)))
       (should (equal (car forms) '(prompt :request 1 :text "hello")))
       (should (equal (cadr forms) '(abort :request 2)))))))

(ert-deftest zerostack-test-latex-preview-is-strictly-inline ()
  (zerostack-test--with-buffer
   (let ((item '(:id "inline-latex"
                     :display nil
                     :source "\\alpha + \\beta"
                     :line-start 0
                     :col-start 2
                     :line-end 0
                     :col-end 18
                     :artifact (:kind latex-source
				      :path "/tmp/math.tex")))
         calls)
     (zerostack--replace-lines
      0
      '((:text "< $\\alpha + \\beta$" :face zs-normal)))
     (cl-letf (((symbol-function 'LaTeX-mode)
                (lambda () (push 'latex-mode calls)))
               ((symbol-function 'TeX-fold-mode)
                (lambda (&optional arg) (push (list 'tex-fold-mode arg) calls)))
               ((symbol-function 'TeX-fold-buffer)
                (lambda () (push 'tex-fold-buffer calls)))
               ((symbol-function 'TeX-fold-buffer-substring)
                (lambda (&rest _) "alpha + beta"))
               ((symbol-function 'display-buffer)
                (lambda (&rest _)
                  (ert-fail "automatic LaTeX preview must stay inline")))
               ((symbol-function 'find-file-noselect)
                (lambda (&rest _)
                  (ert-fail "automatic LaTeX preview must not open source buffers")))
               ((symbol-function 'require)
                (lambda (feature &optional _filename _noerror)
                  (memq feature '(latex tex-fold)))))
       (let ((zerostack-auctex-preview t)
             (zerostack-auctex-fold t)
             (zerostack-auctex-display-buffer t))
         (zerostack--handle-latex-preview-ready (list item))))
     (let ((overlay (cl-find-if (lambda (it)
                                  (overlay-get it 'zerostack-latex))
                                (overlays-in (point-min) (point-max)))))
       (should overlay)
       (should (equal (overlay-get overlay 'display) "alpha + beta"))
       (should (equal (plist-get (overlay-get overlay 'zerostack-artifact) :path)
                      "/tmp/math.tex")))
     (should (member 'latex-mode calls))
     (should (member '(tex-fold-mode 1) calls))
     (should (member 'tex-fold-buffer calls)))))

(ert-deftest zerostack-test-latex-preview-prefers-rust-svg-artifact ()
  (zerostack-test--with-buffer
   (let ((item '(:id "svg-latex"
                     :display nil
                     :source "x^2"
                     :line-start 0
                     :col-start 2
                     :line-end 0
                     :col-end 7
                     :artifact (:kind latex-source :path "/tmp/math.tex")
                     :svg-artifact (:kind latex-svg :path "/tmp/math.svg")))
         calls)
     (zerostack--replace-lines
      0
      '((:text "< $x^2$" :face zs-normal)))
     (cl-letf (((symbol-function 'file-readable-p)
                (lambda (path) (equal path "/tmp/math.svg")))
               ((symbol-function 'image-type-available-p)
                (lambda (type) (eq type 'svg)))
               ((symbol-function 'create-image)
                (lambda (path type &rest _)
                  (push (list 'create-image path type) calls)
                  (list 'image :path path :type type)))
               ((symbol-function 'zerostack--latex-inline-display)
                (lambda (&rest _)
                  (ert-fail "SVG artifact should avoid AUCTeX fallback"))))
       (zerostack--handle-latex-preview-ready (list item)))
     (let ((overlay (cl-find-if (lambda (it)
                                  (overlay-get it 'zerostack-latex))
                                (overlays-in (point-min) (point-max)))))
       (should overlay)
       (should (equal (overlay-get overlay 'display)
                      '(image :path "/tmp/math.svg" :type svg)))
       (should (eq (overlay-get overlay 'face) 'zerostack-latex-face))
       (should (eq (face-attribute 'zerostack-latex-face :inherit nil 'default)
                   'default))
       (should-not (eq (face-attribute 'zerostack-latex-face :underline nil 'default)
                       t))
       (should (equal (plist-get (overlay-get overlay 'zerostack-artifact) :path)
                      "/tmp/math.tex")))
     (should (equal calls '((create-image "/tmp/math.svg" svg)))))))

(ert-deftest zerostack-test-artifact-at-point-opens-file ()
  (zerostack-test--with-buffer
   (let (opened)
     (zerostack--replace-lines
      0
      `((:text "open output" :face zs-link
               :artifact ,zerostack-test--tool-artifact)))
     (goto-char (point-min))
     (cl-letf (((symbol-function 'find-file)
                (lambda (path) (setq opened path))))
       (zerostack-open-artifact-at-point))
     (should (equal opened "/tmp/zerostack-tool.txt")))))

(ert-deftest zerostack-test-end-to-end-socket-roundtrip ()
  (let* ((dir (make-temp-file "zerostack-emacs-e2e" t))
         (socket (expand-file-name "sock" dir))
         (received nil)
         (line-buffer "")
         (server nil)
         (connections nil))
    (cl-labels
        ((send
           (proc form)
           (process-send-string proc (concat (prin1-to-string form) "\n")))
         (request
           (form)
           (plist-get (cdr form) :request))
         (handle
           (proc form)
           (push form received)
           (pcase (car-safe form)
             ('hello
              (send proc `(ready :protocol 1 :session "e2e" :pid 123 :socket ,socket))
              (send proc `(ok :request ,(request form) :protocol 1 :session "e2e"
                              :pid 123 :cols ,(plist-get (cdr form) :cols)
                              :socket ,socket)))
             ('attach
              (send proc '(event :seq 1 :session "e2e" :type session-render
                                 :replace-from 0
                                 :lines ((:text "attached" :face zs-heading)))))
             ('render
              (send proc '(event :seq 2 :session "e2e" :type session-render
                                 :replace-from 0
                                 :lines ((:text "rendered" :face zs-normal)))))
             ('set-view
              (send proc `(ok :request ,(request form)
                              :cols ,(plist-get (cdr form) :cols))))
             ('prompt
              (send proc '(event :seq 3 :session "e2e" :type user-render
                                 :turn 1 :replace-from 1
                                 :lines ((:text "> hi" :face zs-user))))
              (send proc `(event :seq 4 :session "e2e" :type assistant-render
                                 :turn 1 :replace-from 2
                                 :lines ((:text "< answer $x^2$" :face zs-normal
						:artifact ,zerostack-test--tool-artifact
						:latex (,zerostack-test--latex-item)))))
              (send proc `(event :seq 5 :session "e2e" :type tool-result
                                 :turn 1 :name "bash" :chars 17 :preview "ok"
                                 :artifact ,zerostack-test--tool-artifact))
              (send proc `(event :seq 6 :session "e2e" :type latex-preview-ready
                                 :turn 1 :items (,zerostack-test--latex-item)))
              (send proc '(event :seq 7 :session "e2e" :type done
                                 :turn 1 :input-tokens 10 :output-tokens 2)))
             ('compact
              (send proc '(event :seq 8 :session "e2e" :type session-render
                                 :replace-from 0
                                 :lines ((:text "compacted" :face zs-muted))))
              (send proc `(ok :request ,(request form) :compacted nil :messages 0
                              :saved-tokens 0 :message "no-op")))
             ('abort
              (send proc '(event :seq 9 :session "e2e" :type aborted))
              (send proc `(ok :request ,(request form) :aborted nil)))
             ('permission-answer
              (send proc `(event :seq 10 :session "e2e" :type permission-answered
                                 :request ,(request form)
                                 :decision ,(plist-get (cdr form) :decision))))
             ('list-sessions
              (send proc `(sessions :request ,(request form)
                                    :items ((:session "e2e" :pid 123 :cwd ,dir
						      :model "test-model" :provider "test"
						      :created-at "now" :updated-at "now"
						      :title "e2e" :protocol 1 :socket ,socket)))))
             ('status
              (send proc `(status :request ,(request form)
                                  :session (:session "e2e" :pid 123 :cwd ,dir
						     :model "test-model" :provider "test"
						     :created-at "now" :updated-at "now"
						     :title "e2e" :protocol 1 :socket ,socket))))))
         (filter
           (proc chunk)
           (push proc connections)
           (let* ((combined (concat line-buffer chunk))
                  (parts (split-string combined "\n")))
             (setq line-buffer (car (last parts)))
             (dolist (line (butlast parts))
               (unless (string-empty-p line)
                 (handle proc (car (read-from-string line))))))))
      (unwind-protect
          (progn
            (setq server
                  (make-network-process
                   :name "zerostack-e2e-server"
                   :family 'local
                   :service socket
                   :server t
                   :filter #'filter
                   :noquery t))
            (zerostack-test--with-buffer
             (setq zerostack-auctex-preview nil)
             (zerostack--connect-buffer socket)
             (zerostack-test--wait-until
              (lambda () (>= (length received) 3)))

             (zerostack-render)
             (zerostack-set-view 90)
             (zerostack-send-prompt "hi")
             (zerostack-test--wait-until
              (lambda ()
                (and (cl-find "/tmp/zerostack-tool.txt" zerostack--artifacts
                              :key (lambda (artifact) (plist-get artifact :path))
                              :test #'equal)
                     (gethash "turn-1-latex-1" zerostack--latex-items))))
             (zerostack-compact "keep details")
             (zerostack-abort)
             (zerostack-permission-answer 8 'allow-once)
             (zerostack-request-sessions 1)
             (zerostack-request-status)

             (zerostack-test--wait-until
              (lambda () (>= (length received) 11)))
             (zerostack-test--wait-until
              (lambda () (string-match-p "status: e2e" (buffer-string))))

             (let ((forms (nreverse received)))
               (should (equal (mapcar #'car forms)
                              '(hello attach status render set-view prompt compact abort
                                      permission-answer list-sessions status)))
               (should (equal (nth 5 forms) '(prompt :request 6 :text "hi")))
               (should (equal (nth 6 forms)
                              '(compact :request 7 :instructions "keep details")))
               (should (equal (nth 8 forms)
                              '(permission-answer :request 8 :decision allow-once))))
             (should (equal zerostack--session "e2e"))
             (should (string-match-p "\\*zerostack: e2e @ " (buffer-name)))
             (should (string-match-p (regexp-quote (file-name-nondirectory dir))
                                     (buffer-name)))
             (should-not zerostack--artifacts)
             (should (= 0 (hash-table-count zerostack--latex-items)))))
        (dolist (proc (delete-dups connections))
          (when (process-live-p proc)
            (delete-process proc)))
        (when (process-live-p server)
          (delete-process server))
        (delete-directory dir t)))))

(provide 'zerostack-test)

;;; zerostack-test.el ends here
