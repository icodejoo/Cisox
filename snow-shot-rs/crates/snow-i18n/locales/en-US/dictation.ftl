## Dictation (speech to text): status text, output routing, errors and the popup window.

dictation-status-loading = Starting the speech engine and loading the model. The first run can take a while.

dictation-status-listening = Listening. { $route }

dictation-status-finishing = Finishing...

dictation-status-done = Finished.

dictation-route-typing = Typing into the current input box.

dictation-route-typing-overlay = Typing into the current input box and showing it here.

dictation-route-overlay = Showing the text here.

dictation-route-fallback = Typing is not possible ({ $reason }), so the text is shown here.

dictation-route-blocked = Output is set to typing only, but typing is not possible ({ $reason }). The text is shown here instead.

dictation-route-target-lost = The target window changed, so typing stopped. The text is kept here.

dictation-route-send-failed = Typing failed ({ $detail }). The text is kept here.

dictation-route-typing-stuck = The text could not be typed in time (a modifier key may still be held). It is kept here.

dictation-reason-no-focus = there is no input focus

dictation-reason-not-editable = the focused control is not editable

dictation-reason-elevated = the target window runs with higher privileges

dictation-reason-uncertain = it could not be determined whether the target accepts typing

dictation-error-system-online-off = System speech needs "Online speech recognition" turned on. Open Windows Settings > Privacy > Speech (or run ms-settings:privacy-speech), switch it on and try again, or switch to the local model in this app's settings.

dictation-error-system-mic-denied = System speech cannot use the microphone. Open Windows Settings > Privacy > Microphone (ms-settings:privacy-microphone), allow desktop apps to access the microphone, and make sure a default recording device exists. { $detail }

dictation-error-system-language = Windows has no speech recognition support for the required language. Add the language's speech pack in Windows Settings > Time & language > Speech (ms-settings:speech), or change the language in this app's settings to one Windows already has. { $detail }

dictation-error-system-network = Online speech recognition could not connect. Check your network and try again, or switch to the local model in this app's settings.

dictation-error-system-other = System speech recognition failed: { $detail }. Check that "Online speech recognition" is on under Windows Settings > Privacy > Speech (ms-settings:privacy-speech).

dictation-notice-system = System speech uses Windows' built-in recognition, only works with the default microphone, and needs "Online speech recognition" turned on (Windows Settings > Privacy > Speech, ms-settings:privacy-speech).

dictation-error-worker-missing = The speech engine program was not found. Put it next to the app, or set its path in the SNOW_STT_EXE environment variable.

dictation-error-spawn = Could not start the speech engine: { $detail }

dictation-error-model-dir = The speech model folder does not exist: { $path }. Set the folder in settings.

dictation-error-start-timeout = The speech engine did not become ready in time and was stopped.

dictation-error-stop-timeout = The speech engine did not finish in time and was stopped.

dictation-error-crashed = The speech engine exited unexpectedly (exit code { $code }).

dictation-error-worker = Speech engine error: { $detail }

dictation-error-link = Could not talk to the speech engine: { $detail }

dictation-overlay-placeholder = Recognized text appears here. You can edit it.

dictation-overlay-copy = Copy

dictation-overlay-copied = Copied

dictation-overlay-copy-failed = Copy failed: { $reason }

dictation-overlay-close = Close
