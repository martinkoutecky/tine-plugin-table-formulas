;;; -*- lexical-binding: t -*-
;; Emacs `format' oracle for the plugin's printf subset. Input: a JSON array
;; of [FORMAT, TEXT] pairs. Output: a JSON array of the strings
;; (format FORMAT (string-to-number TEXT)), as org-table-eval-formula
;; computes them, or null where Emacs signals an error.
;;   emacs --batch -Q -l tools/emacs-printf-oracle.el pairs.json
(require 'json)
(let* ((pairs (with-temp-buffer
                (insert-file-contents (car command-line-args-left))
                (json-parse-buffer :array-type 'list)))
       (out (mapcar (lambda (p)
                      (condition-case nil
                          (format (nth 0 p) (string-to-number (nth 1 p)))
                        (error :null)))
                    pairs)))
  (princ (json-serialize (vconcat out))))
