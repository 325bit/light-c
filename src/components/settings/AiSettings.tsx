// ============================================================================
// AI assistant settings page
//
// Only handles UI and interaction: network requests, key storage and protocol adaptation are all in the backend,
// so here we **never touch the plaintext API Key** — only the masked form is shown.
// ============================================================================

import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from 'react';
import { useTranslation } from 'react-i18next';
import { AlertCircle, CheckCircle2, Loader2, Plus, Sparkles } from 'lucide-react';
import { Select } from '../ui/Select';
import { Switch } from '../ui/Switch';
import { useToast } from '../Toast';
import { useSettings } from '../../contexts';
import {
  clearAiProfileKey,
  deleteAiProfile,
  getAiConfig,
  listAiProviders,
  saveAiProfile,
  setActiveAiProfile,
  setAiDataDir,
  testAiConnection,
} from '../../api/ai';
import { openInFolder, pickFolderDialog } from '../../api/commands';
import { notifyAiConfigChanged, subscribeAiConfig } from '../ai/aiConfigSignal';
import { ConfirmDialog } from '../ConfirmDialog';
import type { AiProviderPreset, AiPublicConfig } from '../../types/ai';

/** Unified settings row: title and description on the left, control on the right.
 *
 *  ⚠️ In narrow windows the right-side control moves entirely to the next line, instead of squeezing the left text into a thin vertical strip.
 *  Previously it was a rigid two-column layout of "left min-w-0 (infinitely compressible) + right shrink-0 (not compressible)":
 *  on the AI data directory row, the right side has "a long path + three buttons Open/Change/Restore", and when space was insufficient
 *  the left description was squeezed into 4~5 lines (issue screenshot).
 *  Now the container allows wrapping and the left side gets an em-based base width: when wide enough it is still two columns,
 *  and when narrow the whole right block drops to the next line, keeping the description readable. */
function SettingRow({
  title,
  description,
  children,
}: {
  title: string;
  description?: ReactNode;
  children: ReactNode;
}) {
  return (
    <div className="flex flex-wrap items-start justify-between gap-x-4 gap-y-3">
      <div className="min-w-0 flex-1 basis-[10em]">
        <p className="text-sm font-medium text-[var(--text-primary)]">{title}</p>
        {description ? <div className="text-xs text-[var(--text-muted)] mt-1">{description}</div> : null}
      </div>
      <div className="shrink-0 max-w-full">{children}</div>
    </div>
  );
}

/** Text input box with unified styling (currently only used on this page in the settings). */
function TextField({
  value,
  placeholder,
  onChange,
  widthClass = 'w-64',
  password = false,
}: {
  value: string;
  placeholder?: string;
  onChange: (next: string) => void;
  widthClass?: string;
  password?: boolean;
}) {
  return (
    <input
      type={password ? 'password' : 'text'}
      value={value}
      placeholder={placeholder}
      spellCheck={false}
      onChange={(event) => onChange(event.target.value)}
      className={`${widthClass} rounded-xl border border-[var(--border-color)] bg-[var(--bg-card)] px-3 py-2 text-sm text-[var(--text-primary)] outline-none transition-colors focus:border-[var(--brand-green)]`}
    />
  );
}

export function AiSettings() {
  const { t } = useTranslation('settings');
  const { showToast } = useToast();
  const { settings, updateSettings } = useSettings();

  const [providers, setProviders] = useState<AiProviderPreset[]>([]);
  const [saved, setSaved] = useState<AiPublicConfig | null>(null);
  const [modelOptions, setModelOptions] = useState<string[]>([]);
  const [isTesting, setIsTesting] = useState(false);
  const [isSaving, setIsSaving] = useState(false);
  const [errorMessage, setErrorMessage] = useState('');
  const [loaded, setLoaded] = useState(false);

  // Form draft: kept separate from the saved config; changes are not persisted until Save/Test is clicked
  /** Profile display name. Can be left empty — the backend falls back to the provider name, avoiding blank entries in the list. */
  const [profileName, setProfileName] = useState('');
  const [providerId, setProviderId] = useState('deepseek');
  const [baseUrl, setBaseUrl] = useState('');
  const [model, setModel] = useState('');
  // The API key input is always empty: leaving it empty = reuse the saved key (backend convention)
  const [apiKey, setApiKey] = useState('');
  // AI data directory: leaving it empty = follow the main data directory (default location, shown as a placeholder hint).
  // It is a **global setting** and does not belong to any provider — switching providers should not move the chat history along with it.
  const [dataDir, setDataDir] = useState('');
  // The id of the profile currently being edited. All fields in the form belong to it.
  const [activeId, setActiveId] = useState('');
  // Reasoning effort. **Empty by default = do not send this parameter** — there is no way to auto-detect whether the model supports it,
  // and sending it to an unsupported model may cause an error, so it stays quiet by default.
  const [reasoningEffort, setReasoningEffort] = useState('');
  /**
   * Whether the edit area is expanded.
   * The list is **read-only** by default: fields only expand when "Edit" or "Add" is clicked,
   * so it takes no extra space normally and does not give the impression that "casual edits take effect".
   */
  const [isEditing, setIsEditing] = useState(false);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const [presetList, config] = await Promise.all([listAiProviders(), getAiConfig()]);
        if (cancelled) return;
        setProviders(presetList);
        setSaved(config);
        // Initial form values come from the "current profile", not some global field — with multiple profiles, only a profile is a config unit
        const active = config.profiles.find((item) => item.id === config.activeProfileId) ?? config.profiles[0];
        const preset = presetList.find((item) => item.id === active?.providerId);
        setActiveId(active?.id ?? '');
        setProfileName(active?.name ?? '');
        setProviderId(active?.providerId || preset?.id || 'deepseek');
        setBaseUrl(active?.baseUrl || preset?.baseUrl || '');
        setModel(active?.model || '');
        // The model dropdown reads the list cached in the profile directly: it is still there after restart, no need to re-fetch each time
        setModelOptions(active?.models ?? []);
        setReasoningEffort(active?.reasoningEffort ?? '');
        setDataDir(config.dataDir || '');

        // When the model list is empty, fetch it once automatically: users should not have to click "Test Connection" first just to "see which models are available".
        // Failure is silently ignored here — most likely the key has not been filled in or there is no network; clicking "Test Connection" gives an explicit error.
        if (active && active.hasApiKey && active.models.length === 0) {
          void testAiConnection()
            .then((models) => {
              if (cancelled || models.length === 0) return;
              setModelOptions(models);
              // If no model is selected yet, pick the first one as a convenience, saving a step
              if (!active.model) setModel(models[0]);
            })
            .catch(() => undefined);
        }
      } catch (error) {
        if (!cancelled) setErrorMessage(String(error));
      } finally {
        if (!cancelled) setLoaded(true);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  /** Current profile: both the key status and the new profile name come from it — with multiple profiles there is no such thing as a "global key". */
  const activeProfile = useMemo(
    () => saved?.profiles.find((item) => item.id === saved.activeProfileId) ?? saved?.profiles[0],
    [saved],
  );

  const currentPreset = useMemo(
    () => providers.find((item) => item.id === providerId),
    [providers, providerId],
  );

  /** When switching providers, carry the address and default model over as well — the user only needs to fill in one more key. */
  const handleProviderChange = useCallback(
    (nextId: string) => {
      const previousPreset = providers.find((item) => item.id === providerId);
      const preset = providers.find((item) => item.id === nextId);
      setProviderId(nextId);
      if (preset) {
        setBaseUrl(preset.baseUrl);
        setModel(preset.defaultModel);
        // The name follows the provider **only if the user has not changed it**: still empty or still equal to the old preset name means it was not customized.
        // If the user filled it in manually, keep it; do not overwrite the name they chose.
        setProfileName((current) =>
          current.trim() === '' || current === previousPreset?.name ? preset.name : current,
        );
      }
      setModelOptions([]);
      setErrorMessage('');
    },
    [providers, providerId],
  );

  /** Save the draft. When apiKey is empty the backend reuses the saved key. */
  const persist = useCallback(async () => {
    // Key left empty = backend reuses the saved one (the frontend can only get the masked form, not send back the plaintext)
    const next = await saveAiProfile({
      id: activeId,
      // The name the user filled in; if empty, the backend falls back to the provider name (avoids blank entries in the list)
      name: profileName.trim(),
      providerId,
      baseUrl,
      model,
      apiKey,
      reasoningEffort,
    });
    // The data directory is global and saved separately; it does not belong to any profile.
    // **Only write when it actually changed** — otherwise every save adds an extra disk write and an extra "config changed" broadcast.
    const withDir =
      dataDir === (saved?.dataDir ?? '') ? await getAiConfig() : await setAiDataDir(dataDir);
    setSaved(withDir);
    setActiveId(withDir.activeProfileId);
    // Saving fills in the id (when adding a new profile); sync it here, otherwise saving again would insert another copy
    const active = withDir.profiles.find((item) => item.id === withDir.activeProfileId);
    if (active) setActiveId(active.id);
    setApiKey(''); // Clear the input after a successful save, returning to the "masked display" state
    // ⚠️ Saving **does not go through** applyConfig, so we must broadcast once separately here:
    // without broadcasting, the assistant's copy of the config is still stale, and "clicking a quick question right after configuring the Key" would still be wrongly blocked (Evan reported this).
    notifyAiConfigChanged();
    return next;
  }, [activeId, profileName, providerId, baseUrl, model, apiKey, reasoningEffort, dataDir, saved]);

  const handleSave = useCallback(async () => {
    setIsSaving(true);
    setErrorMessage('');
    try {
      await persist();
      setIsEditing(false); // Saving completes it: collapse the edit area, back to the read-only list
      showToast({ type: 'success', title: t('aiAssistant.saved') });
    } catch (error) {
      setErrorMessage(String(error));
    } finally {
      setIsSaving(false);
    }
  }, [persist, showToast, t]);

  /**
   * Test connection = save first, then fetch the model list.
   * It must save first: the backend reads the **saved** config when fetching the list; without saving, it would test with the old config.
   */
  const handleTest = useCallback(async () => {
    setIsTesting(true);
    setErrorMessage('');
    try {
      await persist();
      const models = await testAiConnection();
      setModelOptions(models);
      // If a list was fetched but no model is selected, auto-select the first one, saving a step
      if (!model.trim() && models.length > 0) setModel(models[0]);
      showToast({
        type: 'success',
        title: t('aiAssistant.testOk'),
        description: t('aiAssistant.testOkDesc', { count: models.length }),
      });
    } catch (error) {
      setErrorMessage(String(error));
    } finally {
      setIsTesting(false);
    }
  }, [persist, model, showToast, t]);

  /** Apply a config to the form (used both when switching profiles and when filling in values after saving, keeping a single sync path). */
  const applyConfig = useCallback(
    (config: AiPublicConfig) => {
      const active = config.profiles.find((item) => item.id === config.activeProfileId) ?? config.profiles[0];
      const preset = providers.find((item) => item.id === active?.providerId);
      setSaved(config);
      setActiveId(active?.id ?? '');
      setProfileName(active?.name ?? '');
      setProviderId(active?.providerId || preset?.id || 'deepseek');
      setBaseUrl(active?.baseUrl || preset?.baseUrl || '');
      setModel(active?.model || '');
      setModelOptions(active?.models ?? []);
      setReasoningEffort(active?.reasoningEffort ?? '');
      setApiKey('');
      setErrorMessage('');
      // The config on disk has changed — notify the assistant to re-read it, otherwise its copy is still stale,
      // and clicking a quick question right after configuring a provider is wrongly judged as not configured
      notifyAiConfigChanged();
    },
    [providers],
  );

  /** Mirror of the editing state: read from the subscription callback, so expanding/collapsing the edit area does not re-subscribe every time. */
  const isEditingRef = useRef(isEditing);
  useEffect(() => {
    isEditingRef.current = isEditing;
  }, [isEditing]);

  /**
   * Sync when the config is changed externally: after the assistant panel switches models in place in the input, the model on disk has changed,
   * and if this is not updated the user's subsequent "Save" would write the old model back.
   *
   * ⚠️ Only update state, **do not broadcast again** — persist / applyConfig both broadcast,
   * and broadcasting here would create a notification loop with the settings page itself.
   */
  useEffect(() => subscribeAiConfig(() => {
    void getAiConfig()
      .then((config) => {
        setSaved(config);
        // While editing, preserve the user's draft: only replace the baseline value, do not overwrite the model name they are typing
        if (isEditingRef.current) return;
        const active = config.profiles.find((item) => item.id === config.activeProfileId) ?? config.profiles[0];
        setActiveId(active?.id ?? '');
        setModel(active?.model ?? '');
        setModelOptions(active?.models ?? []);
      })
      .catch((error) => console.error('同步 AI 配置失败:', error));
  }), []);

  /**
   * Whether the form has unsaved changes.
   *
   * Only used to ask first at the three entry points that **lose changes**: "switch profile / add / delete".
   * The criterion is "whether a new key was filled in + whether the four fields differ from the saved values", not timestamps — simple and no false positives.
   */
  const isDirty = useMemo(() => {
    if (apiKey.trim()) return true;
    const current = saved?.profiles.find((item) => item.id === activeId);
    // A draft being newly created has no reference, so it is not treated as "unsaved changes" (the user is editing anyway)
    if (!current) return false;
    return (
      profileName.trim() !== current.name ||
      providerId !== current.providerId ||
      baseUrl !== current.baseUrl ||
      model !== current.model ||
      reasoningEffort !== current.reasoningEffort
    );
  }, [apiKey, saved, activeId, profileName, providerId, baseUrl, model, reasoningEffort]);

  /** Pending "discard changes" action: when there are unsaved changes, park the action here first and execute it after confirmation */
  const [pendingDiscard, setPendingDiscard] = useState<(() => void) | null>(null);

  /** Perform an operation that would lose changes: execute directly if clean, ask first if dirty */
  const confirmUnsaved = useCallback(
    (action: () => void) => {
      if (!isDirty) {
        action();
        return;
      }
      setPendingDiscard(() => action);
    },
    [isDirty],
  );

  const handleSwitchProfile = useCallback(
    async (profileId: string) => {
      if (profileId === activeId) return;
      try {
        applyConfig(await setActiveAiProfile(profileId));
      } catch (error) {
        setErrorMessage(String(error));
      }
    },
    [activeId, applyConfig],
  );

  /**
   * Add: **do not create a profile yet**, just expand a blank form.
   * The profile is actually created and appears in the list only after a successful save — otherwise a half-finished config appears in the list first,
   * and if the user cancels, an empty shell is left behind (Evan reported this).
   *
   * ⚠️ The default provider (DeepSeek)'s **address and default model must be brought out as well**:
   * if only providerId is set, the dropdown shows DeepSeek but the API address is empty,
   * and the user has to switch away and back before it appears (Evan reported this). Reuse the same preset here, consistent with switching.
   */
  const handleAddProfile = useCallback(() => {
    const preset = providers.find((item) => item.id === 'deepseek') ?? providers[0];
    setActiveId('');
    // Prefill the provider name as the initial name; the user can change it to something recognizable; if left empty the backend falls back to the provider name.
    setProfileName(preset?.name ?? '');
    setProviderId(preset?.id ?? 'deepseek');
    setBaseUrl(preset?.baseUrl ?? '');
    setModel(preset?.defaultModel ?? '');
    setModelOptions([]);
    setApiKey('');
    setErrorMessage('');
    setIsEditing(true);
  }, [providers]);

  const handleDeleteProfile = useCallback(
    async (profileId: string) => {
      try {
        applyConfig(await deleteAiProfile(profileId));
      } catch (error) {
        setErrorMessage(String(error));
      }
    },
    [applyConfig],
  );

  /**
   * Remove the saved key for this profile (the profile itself is kept).
   * Revoking a potentially leaked Key is a security operation, so it must be doable on its own — no need to delete the whole profile.
   */
  const handleClearKey = useCallback(async () => {
    if (!activeId) return;
    setIsSaving(true);
    setErrorMessage('');
    try {
      applyConfig(await clearAiProfileKey(activeId));
      setApiKey('');
      showToast({ type: 'success', title: t('aiAssistant.removeKeyDone') });
    } catch (error) {
      setErrorMessage(String(error));
    } finally {
      setIsSaving(false);
    }
  }, [activeId, applyConfig, showToast, t]);

  /**
   * Dropdown options: the fetched models + **the current model itself**.
   *
   * The current value must be included: the model may not be in the fetched results (list not fetched yet, or the model was retired),
   * in which case Select finds no match and shows blank, making the user think the config was lost.
   */
  const modelSelectOptions = useMemo(() => {
    const options = modelOptions.map((id) => ({ value: id, label: id }));
    if (model && !modelOptions.includes(model)) options.unshift({ value: model, label: model });
    return options;
  }, [modelOptions, model]);

  /** Click "Edit" on a row: first switch to it (the form always edits the current profile), then expand the edit area. */
  const handleStartEdit = useCallback(
    async (profileId: string) => {
      if (profileId !== activeId) await handleSwitchProfile(profileId);
      setIsEditing(true);
    },
    [activeId, handleSwitchProfile],
  );

  /** Cancel editing: collapse and **restore the form to its saved state**, discarding these changes. */
  const handleCancelEdit = useCallback(() => {
    if (saved) applyConfig(saved);
    setIsEditing(false);
  }, [saved, applyConfig]);

  /** Open the AI data directory (with the system file manager). If the directory does not exist, show a hint; do not fail silently. */
  const handleOpenDataDir = useCallback(async () => {
    const target = saved?.resolvedDataDir;
    if (!target) return;
    try {
      await openInFolder(target);
    } catch (error) {
      setErrorMessage(String(error));
    }
  }, [saved]);

  /**
   * Select the AI data directory. reset = restore the default location (follow the main data directory).
   * Save **immediately** after selection — after the folder picker closes, making the user hunt for the "Save" button is counter-intuitive.
   */
  const handlePickDataDir = useCallback(
    async (reset: boolean) => {
      try {
        const folder = reset ? '' : await pickFolderDialog();
        if (folder === null) return; // User cancelled
        applyConfig(await setAiDataDir(folder));
      } catch (error) {
        setErrorMessage(String(error));
      }
    },
    [applyConfig],
  );

  return (
    <div className="space-y-6">
      <div className="space-y-3">
        <h4 className="text-xs font-medium text-[var(--text-muted)] uppercase tracking-wider flex items-center gap-2">
          <Sparkles className="w-3.5 h-3.5" />
          {t('aiAssistant.section')}
        </h4>

        {/* Entry display toggle: users who have not configured a provider do not want the floating ball in the lower-right corner and the inline AI button occupying the UI all the time.
            Turning it off only hides the entry; provider config and API keys are all kept (re-enabling restores them immediately). */}
        <div className="glass-block bg-[var(--bg-main)] rounded-2xl px-5 py-3.5 flex items-center justify-between gap-4">
          <div className="min-w-0">
            <p className="text-sm font-medium text-[var(--text-primary)]">{t('aiAssistant.enableEntry')}</p>
            <p className="mt-1 text-xs text-[var(--text-muted)]">{t('aiAssistant.enableEntryDesc')}</p>
          </div>
          <Switch
            checked={settings.aiAssistantEnabled}
            onChange={(checked) => updateSettings({ aiAssistantEnabled: checked })}
            aria-label={t('aiAssistant.enableEntry')}
          />
        </div>

        <div className="glass-block bg-[var(--bg-main)] rounded-2xl p-5 space-y-5">
          {/* Provider list: following the deepseek harness — a vertical list, one delete per row,
              with a full-width "Add Provider" row at the bottom.
              ⚠️ Deletion is allowed **even when only one remains**: previously the delete button was hidden under a "keep at least one" rule,
              so to clear it the user had to add one first and then delete it, which was very awkward (Evan reported this). */}
          <div className="space-y-1.5">
            {saved?.profiles.map((profile) => {
              const isActive = profile.id === saved.activeProfileId;
              const label =
                profile.name ||
                providers.find((item) => item.id === profile.providerId)?.name ||
                profile.providerId ||
                t('aiAssistant.profileUnnamed');
              return (
                <div
                  key={profile.id}
                  className={`flex items-center gap-2 rounded-xl border px-3 py-1.5 transition-colors ${
                    isActive
                      ? 'border-[var(--brand-green)] bg-[var(--brand-green-10)]'
                      : 'border-[var(--border-color)] hover:bg-[var(--bg-hover)]'
                  }`}
                >
                  <button
                    type="button"
                    onClick={() => confirmUnsaved(() => void handleSwitchProfile(profile.id))}
                    className="flex min-w-0 flex-1 items-center gap-2 text-left"
                  >
                    {/* Dot = whether a key is configured, so you can tell at a glance which entry is usable */}
                    <span
                      className={`h-1.5 w-1.5 shrink-0 rounded-full ${
                        profile.hasApiKey ? 'bg-[var(--brand-green)]' : 'bg-[var(--text-muted)]'
                      }`}
                      title={profile.hasApiKey ? t('aiAssistant.profileReady') : t('aiAssistant.profileNoKey')}
                    />
                    <span className="truncate text-sm text-[var(--text-primary)]">{label}</span>
                    {profile.model ? (
                      <span className="truncate text-xs text-[var(--text-muted)]">· {profile.model}</span>
                    ) : null}
                  </button>
                  <button
                    type="button"
                    onClick={() => void handleStartEdit(profile.id)}
                    className="shrink-0 rounded-lg border border-[var(--border-color)] px-2.5 py-1 text-xs text-[var(--text-secondary)] transition-colors hover:bg-[var(--bg-hover)]"
                  >
                    {t('aiAssistant.profileEdit')}
                  </button>
                  <button
                    type="button"
                    onClick={() => confirmUnsaved(() => void handleDeleteProfile(profile.id))}
                    className="shrink-0 rounded-lg px-2 py-1 text-xs text-[var(--text-muted)] transition-colors hover:bg-[var(--color-danger)]/10 hover:text-[var(--color-danger)]"
                  >
                    {t('aiAssistant.profileDelete')}
                  </button>
                </div>
              );
            })}
            <button
              type="button"
              onClick={() => confirmUnsaved(() => handleAddProfile())}
              className="flex w-full items-center justify-center gap-1.5 rounded-xl border border-dashed border-[var(--border-color)] py-2 text-xs text-[var(--text-muted)] transition-colors hover:bg-[var(--bg-hover)]"
            >
              <Plus className="h-3.5 w-3.5" />
              {t('aiAssistant.profileAdd')}
            </button>
          </div>

          {isEditing ? (
          <>
          {/* Name goes above the provider: first give this config a name, then see which provider it connects to.
              If left empty it falls back to the provider name, so no blank entries appear in the list. */}
          <SettingRow title={t('aiAssistant.profileName')} description={t('aiAssistant.profileNameDesc')}>
            <TextField
              value={profileName}
              onChange={setProfileName}
              placeholder={currentPreset?.name}
              widthClass="w-64"
            />
          </SettingRow>

          <div className="border-t border-[var(--border-color)] pt-5">
          <SettingRow title={t('aiAssistant.provider')} description={t('aiAssistant.providerDesc')}>
            <Select<string>
              value={providerId}
              options={providers.map((item) => ({ value: item.id, label: item.name }))}
              onChange={handleProviderChange}
              widthClass="w-64"
            />
          </SettingRow>
          </div>

          <div className="border-t border-[var(--border-color)] pt-5">
            <SettingRow title={t('aiAssistant.baseUrl')} description={t('aiAssistant.baseUrlDesc')}>
              <TextField value={baseUrl} onChange={setBaseUrl} widthClass="w-64" />
            </SettingRow>
          </div>

          <div className="border-t border-[var(--border-color)] pt-5">
            <SettingRow
              title={t('aiAssistant.apiKey')}
              description={
                activeProfile?.hasApiKey
                  ? t('aiAssistant.apiKeySaved', { masked: activeProfile?.apiKeyMasked ?? '' })
                  : currentPreset?.local
                    // Local inference (Ollama / LM Studio / vLLM) needs no authentication, so the hint can be left empty,
                    // otherwise users would think it is required and get stuck at this step.
                    ? t('aiAssistant.apiKeyOptionalDesc')
                    : t('aiAssistant.apiKeyDesc')
              }
            >
              <div className="flex items-center gap-2">
                <TextField
                  value={apiKey}
                  onChange={setApiKey}
                  placeholder={
                    activeProfile?.hasApiKey
                      ? t('aiAssistant.apiKeyKeep')
                      : currentPreset?.local
                        ? t('aiAssistant.apiKeyOptionalHint')
                        : 'sk-...'
                  }
                  widthClass="w-64"
                  password
                />
                {/* Only shown when configured: revoking a potentially leaked Key must be possible, but must not be triggered by accidentally clicking a blank button */}
                {activeProfile?.hasApiKey ? (
                  <button
                    type="button"
                    onClick={() => void handleClearKey()}
                    disabled={isSaving}
                    className="shrink-0 rounded-lg px-2 py-1 text-xs text-[var(--color-danger)] transition-colors hover:bg-[var(--bg-hover)] disabled:opacity-50"
                  >
                    {t('aiAssistant.removeKey')}
                  </button>
                ) : null}
              </div>
            </SettingRow>
          </div>

          <div className="border-t border-[var(--border-color)] pt-5">
            <SettingRow title={t('aiAssistant.model')} description={t('aiAssistant.modelDesc')}>
              {/* Use the project's own Select (the same component as "Language" in general settings),
                  rather than building a dropdown — previously with a native datalist it filtered candidates by what was already typed,
                  so the current value filtered out all other candidates, making it look like "nothing can be selected". */}
              {modelSelectOptions.length > 0 ? (
                <Select<string>
                  value={model}
                  options={modelSelectOptions}
                  onChange={setModel}
                  widthClass="w-64"
                />
              ) : (
                /* If even the current value is missing (never configured and no list fetched): fall back to a text input,
                   to avoid showing an empty dropdown that cannot be clicked. */
                <TextField
                  value={model}
                  onChange={setModel}
                  placeholder={currentPreset?.defaultModel || 'model-name'}
                  widthClass="w-64"
                />
              )}
            </SettingRow>
          </div>

          {/* Reasoning effort: only sent when the user actively selects it.
              ⚠️ There is no way to auto-detect model support (the model list does not return capability info),
              so it is left empty and not sent by default — more reliable than guessing for the user. */}
          <div className="border-t border-[var(--border-color)] pt-5">
            <SettingRow
              title={t('aiAssistant.reasoningEffort')}
              description={t('aiAssistant.reasoningEffortDesc')}
            >
              <Select<string>
                value={reasoningEffort}
                options={[
                  { value: '', label: t('aiAssistant.reasoningDefault') },
                  { value: 'low', label: 'low' },
                  { value: 'medium', label: 'medium' },
                  { value: 'high', label: 'high' },
                ]}
                onChange={setReasoningEffort}
                widthClass="w-64"
              />
            </SettingRow>
          </div>


          </>) : null}


          {/* The action row also only appears in edit mode: seeing "Save" when not editing would suggest there are unsaved changes */}
          {isEditing ? (
          <div className="border-t border-[var(--border-color)] pt-5 flex items-center justify-between gap-4">
            <div className="min-w-0 text-xs">
              {currentPreset?.local ? (
                <span className="flex items-center gap-1.5 text-[var(--brand-green)]">
                  <CheckCircle2 className="w-3.5 h-3.5 shrink-0" />
                  {t('aiAssistant.localHint')}
                </span>
              ) : (
                <span className="text-[var(--text-muted)]">{t('aiAssistant.remoteHint')}</span>
              )}
            </div>
            <div className="flex shrink-0 items-center gap-2">
              <button
                type="button"
                onClick={handleCancelEdit}
                className="rounded-lg border border-[var(--border-color)] px-3 py-1.5 text-xs text-[var(--text-secondary)] transition-colors hover:bg-[var(--bg-hover)]"
              >
                {t('aiAssistant.profileCancel')}
              </button>
              <button
                type="button"
                onClick={() => void handleTest()}
                disabled={isTesting || isSaving}
                className="inline-flex items-center gap-1.5 rounded-lg border border-[var(--brand-green-20)] px-3 py-1.5 text-xs font-medium text-[var(--brand-green)] transition-colors hover:bg-[var(--brand-green-10)] disabled:cursor-not-allowed disabled:opacity-50"
              >
                {isTesting ? <Loader2 className="w-3.5 h-3.5 animate-spin" /> : null}
                {isTesting ? t('aiAssistant.testing') : t('aiAssistant.test')}
              </button>
              <button
                type="button"
                onClick={() => void handleSave()}
                disabled={isSaving || isTesting}
                className="inline-flex items-center gap-1.5 rounded-lg bg-[var(--brand-green)] px-3 py-1.5 text-xs font-medium text-white transition-colors hover:bg-[var(--brand-green-hover)] disabled:cursor-not-allowed disabled:opacity-50"
              >
                {isSaving ? <Loader2 className="w-3.5 h-3.5 animate-spin" /> : null}
                {t('aiAssistant.save')}
              </button>
            </div>
          </div>
          ) : null}

          {errorMessage ? (
            <div className="flex items-start gap-2 rounded-xl border border-[var(--color-danger)]/30 bg-[var(--color-danger)]/5 p-3 text-xs text-[var(--color-danger)]">
              <AlertCircle className="mt-0.5 w-3.5 h-3.5 shrink-0" />
              <span className="min-w-0 break-all">{errorMessage}</span>
            </div>
          ) : null}

          {!loaded ? (
            <p className="text-xs text-[var(--text-muted)]">{t('aiAssistant.loading')}</p>
          ) : null}
        </div>
      </div>

      {/* AI data directory: a **global setting**, not belonging to any provider profile, so it gets its own card.
          The provider card answers "which model to use", this one answers "where things are stored" — two different things. */}
      <div className="glass-block bg-[var(--bg-main)] rounded-2xl p-5">
        <SettingRow title={t('aiAssistant.dataDir')} description={t('aiAssistant.dataDirDesc')}>
          {/* The path + buttons are also allowed to wrap: in narrow windows the three buttons move to the line below the path,
              rather than the whole block overflowing the card. The path needs min-w-0 to truncate properly. */}
          <div className="flex flex-wrap items-center justify-end gap-2">
            {/* Show the resolved **real path** directly: what the user asks is "where exactly is it stored",
                and just writing "the ai folder under the data directory" is no answer at all. */}
            <span
              className="min-w-0 max-w-[20em] truncate text-xs text-[var(--text-muted)]"
              title={saved?.resolvedDataDir || ''}
            >
              {saved?.resolvedDataDir || t('aiAssistant.dataDirDefault')}
            </span>
            <button
              type="button"
              onClick={() => void handleOpenDataDir()}
              className="shrink-0 rounded-xl border border-[var(--border-color)] px-3 py-2 text-sm text-[var(--text-secondary)] transition-colors hover:bg-[var(--bg-hover)]"
            >
              {t('aiAssistant.dataDirOpen')}
            </button>
            <button
              type="button"
              onClick={() => void handlePickDataDir(false)}
              className="shrink-0 rounded-xl border border-[var(--border-color)] px-3 py-2 text-sm text-[var(--text-secondary)] transition-colors hover:bg-[var(--bg-hover)]"
            >
              {t('aiAssistant.dataDirPick')}
            </button>
            {dataDir ? (
              <button
                type="button"
                onClick={() => void handlePickDataDir(true)}
                className="shrink-0 rounded-xl px-2 py-2 text-xs text-[var(--text-muted)] transition-colors hover:bg-[var(--bg-hover)]"
              >
                {t('aiAssistant.dataDirReset')}
              </button>
            ) : null}
          </div>
        </SettingRow>
      </div>

      <p className="text-xs text-[var(--text-muted)] leading-relaxed">{t('aiAssistant.privacyNote')}</p>

      {/* Only block once when there are unsaved changes: switching profile / adding / deleting all wipe the current form */}
      <ConfirmDialog
        isOpen={pendingDiscard !== null}
        title={t('aiAssistant.unsavedTitle')}
        description={t('aiAssistant.unsavedDesc')}
        confirmText={t('aiAssistant.unsavedDiscard')}
        onConfirm={() => {
          const action = pendingDiscard;
          setPendingDiscard(null);
          action?.();
        }}
        onCancel={() => setPendingDiscard(null)}
      />
    </div>
  );
}
