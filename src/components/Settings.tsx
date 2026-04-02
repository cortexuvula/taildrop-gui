import type { Peer, AppSettings } from "../types";

interface SettingsProps {
  settings: AppSettings;
  allPeers: Peer[];
  onUpdate: (update: Partial<AppSettings>) => void;
  onClose: () => void;
}

export function Settings({ settings, allPeers, onUpdate, onClose }: SettingsProps) {
  const nonSelfPeers = allPeers.filter((p) => !p.is_self);

  const toggleHidden = (id: string) => {
    const hidden = settings.hiddenNodes.includes(id)
      ? settings.hiddenNodes.filter((h) => h !== id)
      : [...settings.hiddenNodes, id];
    onUpdate({ hiddenNodes: hidden });
  };

  return (
    <div className="settings-overlay" onClick={onClose}>
      <div className="settings-panel" onClick={(e) => e.stopPropagation()}>
        <div className="settings-header">
          <h2>Settings</h2>
          <button className="icon-btn" onClick={onClose}>
            ✕
          </button>
        </div>

        <div className="settings-section">
          <label className="settings-label">Save Directory</label>
          <input
            type="text"
            className="settings-input"
            value={settings.saveDirectory}
            onChange={(e) => onUpdate({ saveDirectory: e.target.value })}
            placeholder="Downloads folder"
          />
        </div>

        <div className="settings-section">
          <label className="settings-label toggle-row">
            <span>Auto-accept incoming files</span>
            <input
              type="checkbox"
              checked={settings.autoAccept}
              onChange={(e) => onUpdate({ autoAccept: e.target.checked })}
            />
          </label>
        </div>

        <div className="settings-section">
          <label className="settings-label toggle-row">
            <span>Show offline nodes</span>
            <input
              type="checkbox"
              checked={settings.showOfflineNodes ?? false}
              onChange={(e) => onUpdate({ showOfflineNodes: e.target.checked })}
            />
          </label>
        </div>

        <div className="settings-section">
          <label className="settings-label toggle-row">
            <span>Show Mullvad/exit nodes</span>
            <input
              type="checkbox"
              checked={settings.showExitNodes ?? false}
              onChange={(e) => onUpdate({ showExitNodes: e.target.checked })}
            />
          </label>
        </div>

        <div className="settings-section">
          <label className="settings-label">Node Visibility</label>
          <div className="node-visibility-list">
            {nonSelfPeers.map((peer) => (
              <label key={peer.id || peer.public_key} className="toggle-row">
                <span>
                  {peer.hostname}
                  <span className="peer-os-small">{peer.os}</span>
                </span>
                <input
                  type="checkbox"
                  checked={!settings.hiddenNodes.includes(peer.id)}
                  onChange={() => toggleHidden(peer.id)}
                />
              </label>
            ))}
            {nonSelfPeers.length === 0 && (
              <div className="empty-state">No peers discovered yet</div>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
