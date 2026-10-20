;;; -*- lexical-binding: t -*-
(require 'org) (require 'org-table) (require 'json)
;; The input is a list of {id, input} cases, or a fixture file written by
;; this script ({revision, cases}), whose cases are recomputed.
(let* ((parsed (with-temp-buffer (insert-file-contents (car command-line-args-left)) (json-parse-buffer :object-type 'alist :array-type 'list)))
       (cases (if (assq 'cases parsed) (alist-get 'cases parsed) parsed))
       (out '()))
  (dolist (c cases)
    (let* ((id (alist-get 'id c)) (input (alist-get 'input c)) res err)
      (with-temp-buffer
        (org-mode)
        (insert input)
        (goto-char (point-min))
        (re-search-forward "^[ \t]*#\\+TBLFM:")
        (condition-case e
            (progn (org-ctrl-c-ctrl-c) (setq res (buffer-string)))
          (error (setq err (error-message-string e)))))
      (push (if err `((id . ,id) (input . ,input) (laps . 1) (org_error . ,err))
              `((id . ,id) (input . ,input) (laps . 1) (expected . ,res))) out)))
  (princ (json-encode `((revision . ,(format "Emacs %s, bundled Org %s" emacs-version (org-version))) (cases . ,(vconcat (nreverse out)))))))
