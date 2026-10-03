import { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Download, LoaderCircle, LogIn } from 'lucide-react';
import qoderIcon from '../assets/icons/qoder.svg';
import { useI18n } from '../i18n';

/**
 * Qoder is not an OAuth provider. Its credentials live in a separate CLI that
 * keeps its own session, so this card cannot ask for a URL and poll: it has to
 * find the binary, offer to install it, and then start a sign-in the user
 * completes themselves.
 *
 * Both actions run in a terminal window the user can see, because signing in
 * opens a browser and waits. This card therefore polls for the result rather
 * than waiting on the process it started.
 */

type QoderCliStatus = {
  installed: boolean;
  path?: string | null;
  version?: string | null;
  models: string[];
};

const POLL_INTERVAL_MS = 3000;
/** Long enough for a 217 MB download on a slow connection. */
const INSTALL_POLL_DEADLINE_MS = 10 * 60 * 1000;
/** Long enough for a browser round trip and a cold model listing. */
const LOGIN_POLL_DEADLINE_MS = 5 * 60 * 1000;

export function QoderCliCard() {
  const { t } = useI18n();
  const [status, setStatus] = useState<QoderCliStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const mounted = useRef(true);
  const timer = useRef<number | null>(null);

  const readStatus = useCallback(
    (checkModels: boolean) => invoke<QoderCliStatus>('qoder_cli_status', { checkModels }),
    [],
  );

  const stopPolling = useCallback(() => {
    if (timer.current !== null) {
      window.clearTimeout(timer.current);
      timer.current = null;
    }
  }, []);

  useEffect(() => () => {
    mounted.current = false;
    stopPolling();
  }, [stopPolling]);

  useEffect(() => {
    let active = true;
    // The cheap probe first, so the card is not blank while the model listing
    // runs: that listing shells out and can take seconds.
    void readStatus(false)
      .then((next) => { if (active) setStatus(next); })
      .catch(() => undefined)
      .then(() => readStatus(true))
      .then((next) => { if (active) setStatus(next); })
      .catch(() => undefined);
    return () => { active = false; };
  }, [readStatus]);

  const pollUntil = useCallback((
    predicate: (next: QoderCliStatus) => boolean,
    deadlineMs: number,
    giveUpMessage: string,
  ) => {
    const deadline = Date.now() + deadlineMs;
    const tick = () => {
      void readStatus(true)
        .then((next) => {
          if (!mounted.current) return;
          setStatus(next);
          if (predicate(next)) {
            stopPolling();
            setBusy(false);
            return;
          }
          if (Date.now() > deadline) {
            stopPolling();
            setBusy(false);
            setError(giveUpMessage);
            return;
          }
          timer.current = window.setTimeout(tick, POLL_INTERVAL_MS);
        })
        .catch(() => {
          if (!mounted.current) return;
          stopPolling();
          setBusy(false);
        });
    };
    timer.current = window.setTimeout(tick, POLL_INTERVAL_MS);
  }, [readStatus, stopPolling]);

  const handleInstall = useCallback(() => {
    // Installing runs a script fetched from the internet. Name the host so the
    // confirmation says what is being trusted.
    if (!window.confirm(t('oauth.qoderConfirmInstall'))) return;
    setError(null);
    setBusy(true);
    void invoke('qoder_cli_install')
      .then(() => pollUntil(
        (next) => next.installed,
        INSTALL_POLL_DEADLINE_MS,
        t('oauth.qoderInstallUnfinished'),
      ))
      .catch((cause) => {
        if (!mounted.current) return;
        setBusy(false);
        setError(String(cause));
      });
  }, [pollUntil, t]);

  const handleLogin = useCallback(() => {
    setError(null);
    setBusy(true);
    void invoke('qoder_cli_login')
      .then(() => pollUntil(
        (next) => next.models.length > 0,
        LOGIN_POLL_DEADLINE_MS,
        t('oauth.qoderSignInUnfinished'),
      ))
      .catch((cause) => {
        if (!mounted.current) return;
        setBusy(false);
        setError(String(cause));
      });
  }, [pollUntil, t]);

  const installed = status?.installed ?? false;
  const signedIn = (status?.models.length ?? 0) > 0;

  return (
    <section className="panel oauth-card">
      <div className="provider-title-row">
        <img src={qoderIcon} alt="" className="provider-logo" />
        <div>
          <h2>{t('oauth.qoderTitle')}</h2>
          {signedIn ? <span className="state-pill success">{t('oauth.qoderSignedIn')}</span> : null}
        </div>
      </div>

      <div className="oauth-card-body">
        <p className="oauth-hint">
          {installed ? t('oauth.qoderHintInstalled') : t('oauth.qoderHintMissing')}
        </p>
        {status?.version ? (
          <p className="oauth-hint">{t('oauth.qoderVersion')} {status.version}</p>
        ) : null}
        {error ? <p className="oauth-hint">{error}</p> : null}
      </div>

      <div className="button-row management-card-actions">
        {installed ? (
          <button type="button" className="primary-button" disabled={busy} onClick={handleLogin}>
            {busy ? <LoaderCircle size={16} className="spin" aria-hidden="true" /> : <LogIn size={16} aria-hidden="true" />}
            {signedIn ? t('oauth.qoderLoginAgain') : t('oauth.qoderLogin')}
          </button>
        ) : (
          <button type="button" className="primary-button" disabled={busy} onClick={handleInstall}>
            {busy ? <LoaderCircle size={16} className="spin" aria-hidden="true" /> : <Download size={16} aria-hidden="true" />}
            {busy ? t('oauth.qoderInstalling') : t('oauth.qoderInstall')}
          </button>
        )}
      </div>
    </section>
  );
}
