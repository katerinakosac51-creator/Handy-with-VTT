import React, { useState, useEffect } from "react";
import { useTranslation } from "react-i18next";
import { useSettings } from "../../hooks/useSettings";
import { ToggleSwitch } from "../ui/ToggleSwitch";
import { Input } from "../ui/Input";
import { SettingContainer } from "../ui/SettingContainer";
import { Button } from "../ui/Button";
import { listen } from "@tauri-apps/api/event";

interface WakeWordSettingsProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

export const WakeWordSettings: React.FC<WakeWordSettingsProps> = React.memo(
  ({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const { getSetting, updateSetting, isUpdating } = useSettings();

    const alwaysOn = getSetting("always_on_microphone") || false;
    const enabled = getSetting("wake_word_enabled") || false;

    const savedPhrase = getSetting("wake_word_phrase") ?? "hey handy";
    const savedStop = getSetting("wake_word_stop_phrase") ?? "stop recording";

    const [phraseInput, setPhraseInput] = useState(savedPhrase);
    const [stopInput, setStopInput] = useState(savedStop);
    const [audioLevels, setAudioLevels] = useState<number[]>(new Array(16).fill(0));
    const [isListening, setIsListening] = useState(false);

    // Listen for mic-level updates from the always-on stream
    useEffect(() => {
      if (!enabled || !alwaysOn) return;

      const unlistenLevel = listen<number[]>("mic-level", (event) => {
        setAudioLevels(event.payload);
      });

      const unlistenDetected = listen("wake-word-detected", () => {
        setIsListening(true);
        setTimeout(() => setIsListening(false), 2000);
      });

      return () => {
        unlistenLevel.then((fn) => fn());
        unlistenDetected.then((fn) => fn());
      };
    }, [enabled, alwaysOn]);

    const commitPhrase = () => {
      const trimmed = phraseInput.trim();
      if (trimmed && trimmed !== savedPhrase) {
        updateSetting("wake_word_phrase", trimmed);
      }
    };

    const commitStop = () => {
      const trimmed = stopInput.trim();
      if (trimmed && trimmed !== savedStop) {
        updateSetting("wake_word_stop_phrase", trimmed);
      }
    };

    // Calculate average level for display
    const avgLevel = audioLevels.reduce((a, b) => a + b, 0) / (audioLevels.length || 1);
    const hasAudio = avgLevel > 0.02;

    const isDisabled = !alwaysOn || !enabled || isListening || isUpdating("wake_word_phrase") || isUpdating("wake_word_stop_phrase");

    return (
      <div className="space-y-4">
        <ToggleSwitch
          checked={enabled}
          onChange={(v) => updateSetting("wake_word_enabled", v)}
          isUpdating={isUpdating("wake_word_enabled")}
          disabled={!alwaysOn}
          label={t("settings.advanced.wakeWord.label")}
          description={
            alwaysOn
              ? t("settings.advanced.wakeWord.description")
              : t("settings.advanced.wakeWord.requiresAlwaysOn")
          }
          descriptionMode={descriptionMode}
          grouped={grouped}
        />

        {enabled && alwaysOn && (
          <>
            {/* Voice Bar Display */}
            <div className="py-3 px-4 bg-muted/30 rounded-lg">
              <div className="flex items-center justify-between mb-2">
                <span className="text-sm font-medium">{t("settings.advanced.wakeWord.voiceBarLabel") || "Voice Detection"}</span>
                <span className={`text-xs ${hasAudio ? "text-green-500" : "text-muted-foreground"}`}>
                  {hasAudio ? "● " : "○ "} {isListening ? (t("settings.advanced.wakeWord.listeningForPhrase") || "Listening...") : (hasAudio ? t("settings.advanced.wakeWord.audioDetected") || "Audio detected" : t("settings.advanced.wakeWord.listening") || "Idle")}
                </span>
              </div>
              <div className="flex items-end gap-0.5 h-12">
                {audioLevels.map((level, i) => (
                  <div
                    key={i}
                    className="flex-1 bg-gradient-to-t from-primary/60 to-primary rounded-t transition-all duration-75"
                    style={{
                      height: `${Math.max(4, level * 100)}%`,
                      opacity: 0.3 + (level * 0.7),
                    }}
                  />
                ))}
              </div>
            </div>

            <SettingContainer
              title={t("settings.advanced.wakeWord.startPhraseLabel")}
              description={t("settings.advanced.wakeWord.startPhraseDescription")}
              descriptionMode={descriptionMode}
              grouped={grouped}
              layout="stacked"
            >
              <div className="flex gap-2">
                <Input
                  value={phraseInput}
                  onChange={(e) => setPhraseInput(e.target.value)}
                  onKeyDown={(e) => e.key === "Enter" && commitPhrase()}
                  placeholder={t("settings.advanced.wakeWord.startPhrasePlaceholder")}
                  className="flex-1"
                  disabled={isDisabled}
                />
                <Button onClick={commitPhrase} disabled={isDisabled || phraseInput.trim() === savedPhrase}>
                  {t("common.save") || "Save"}
                </Button>
              </div>
            </SettingContainer>

            <SettingContainer
              title={t("settings.advanced.wakeWord.stopPhraseLabel")}
              description={t("settings.advanced.wakeWord.stopPhraseDescription")}
              descriptionMode={descriptionMode}
              grouped={grouped}
              layout="stacked"
            >
              <div className="flex gap-2">
                <Input
                  value={stopInput}
                  onChange={(e) => setStopInput(e.target.value)}
                  onKeyDown={(e) => e.key === "Enter" && commitStop()}
                  placeholder={t("settings.advanced.wakeWord.stopPhrasePlaceholder")}
                  className="flex-1"
                  disabled={isDisabled}
                />
                <Button onClick={commitStop} disabled={isDisabled || stopInput.trim() === savedStop}>
                  {t("common.save") || "Save"}
                </Button>
              </div>
            </SettingContainer>
          </>
        )}
      </div>
    );
  },
);
