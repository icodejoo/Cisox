## Download, unpack and install errors and steps (OCR / onnxruntime / speech models).

fetch-run-tool = Could not run { $tool }: { $detail }

fetch-wait-tool = Waiting for { $tool } failed: { $detail }

fetch-hash-unreadable = Could not read the hash of { $path }.

fetch-stat-failed = Could not read { $name }: { $detail }

fetch-size-mismatch = { $name } has the wrong size: expected { $expected }, got { $actual }.

fetch-hash-mismatch = { $name } failed the hash check: expected { $expected }, got { $actual }.

fetch-download-failed = Download failed ({ $url }): { $detail }

fetch-cancelled = Cancelled.

fetch-create-dir-failed = Could not create the folder: { $detail }

fetch-rename-failed = Could not rename the file: { $detail }

fetch-extract-failed = Could not unpack the archive: { $detail }

fetch-marker-failed = Could not write the completion marker: { $detail }

fetch-invalid-target = The target path is not valid.

fetch-meta-serialize-failed = Could not serialize the model metadata: { $detail }

fetch-meta-write-failed = Could not write model.json: { $detail }

fetch-staging-failed = Could not create the staging folder: { $detail }

fetch-missing-files = The archive is missing required files: { $files }

fetch-swap-failed = Could not move the model folder into place: { $detail }

fetch-install-member-failed = Could not install { $name }: { $detail }

fetch-manifest-corrupt = The built-in { $what } manifest is damaged: { $detail }

fetch-step-ocr-runtime-download = Downloading the OCR runtime...

fetch-step-ocr-runtime-extract = Unpacking the OCR runtime...

fetch-step-ocr-model = Downloading the OCR model ({ $index }/{ $total })...

fetch-step-ort-download = Downloading the onnxruntime runtime...

fetch-step-ort-extract = Unpacking the onnxruntime runtime...

ocr-unavailable-no-runtime = The OCR runtime is not installed. Download it first (about 17 MB).

ocr-unavailable-no-model = The OCR model { $id } is not installed. Download it first.

ocr-unavailable-unknown-model = Unknown OCR model type: { $kind }

ocr-err-spawn-failed = Could not start the OCR process: { $detail }

ocr-err-ready-timeout = The OCR process took too long to start.

ocr-err-version-mismatch = The OCR runtime version does not match (needs protocol { $expected }, found { $actual }). Download the runtime again.

ocr-err-died = The OCR process exited unexpectedly.

ocr-err-died-detail = The OCR process exited unexpectedly: { $detail }

ocr-err-session-not-ready = The OCR model failed to load (the model files may be damaged or onnxruntime is missing). Download the model again.

ocr-err-failed = Recognition failed: { $detail }

ocr-err-cancelled = Recognition was cancelled.

ocr-err-timeout = OCR timed out ({ $step })

ocr-err-protocol = OCR communication error: { $detail }

ocr-err-io = OCR temporary file error: { $detail }

ocr-err-invalid-image = This image cannot be recognized: { $detail }

ocr-step-load-model = loading the model

ocr-step-shutdown = shutting down

ocr-step-attach-image = attaching the image

ocr-step-transfer-image = sending the image

ocr-step-recognize = recognizing

ocr-step-detach = releasing the image

ocr-panel-running = Recognizing text...

ocr-panel-empty = No text found

ocr-panel-done-copied = Recognized { $count } lines; the text was copied to the clipboard

ocr-panel-done-copy-failed = Recognized { $count } lines (copying to the clipboard failed)

ocr-panel-press-d = { $message } - press D to download

ocr-panel-downloading-hint = After the download finishes, click "OCR" again.

ocr-panel-esc = Esc to go back

ocr-panel-more-lines = ... ({ $count } more lines)

ocr-panel-footer-copied = Copied - E opens the result window - Enter copies and closes - Esc goes back

ocr-panel-footer-copy-failed = Copy failed - E opens the result window - Enter retries and closes - Esc goes back

ocr-panel-download-hint = Press D to download the OCR components (runtime about 17 MB + model about 31 MB)

fetch-task-start-failed = Could not start the background task: { $detail }

ocr-panel-done-not-copied = Recognized { $count } lines; press Enter to copy the text

ocr-panel-footer-not-copied = Not copied - E opens the result window - Enter copies and closes - Esc goes back
