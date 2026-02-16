// Browser notification support for alerts and task completion

let permissionGranted = false;

export async function requestNotificationPermission(): Promise<boolean> {
  if (!('Notification' in window)) return false;
  if (Notification.permission === 'granted') {
    permissionGranted = true;
    return true;
  }
  if (Notification.permission === 'denied') return false;
  const result = await Notification.requestPermission();
  permissionGranted = result === 'granted';
  return permissionGranted;
}

export function sendNotification(title: string, body: string, options?: { tag?: string; icon?: string }) {
  if (!('Notification' in window)) return;
  if (Notification.permission !== 'granted') return;
  // Don't notify if window is focused
  if (document.hasFocus()) return;

  try {
    const notification = new Notification(title, {
      body,
      icon: options?.icon || '/icon.svg',
      tag: options?.tag,
      silent: false,
    });

    notification.onclick = () => {
      window.focus();
      notification.close();
    };

    // Auto-close after 8 seconds
    setTimeout(() => notification.close(), 8000);
  } catch {
    // Notification API not available in this context
  }
}

export function notifyAlertTriggered(alertName: string, value?: number) {
  sendNotification(
    'Alert Triggered',
    `${alertName}${value !== undefined ? ` — Value: ${value}` : ''}`,
    { tag: `alert-${alertName}` }
  );
}

export function notifyTaskCompleted(taskLabel: string, success: boolean) {
  sendNotification(
    success ? 'Task Completed' : 'Task Failed',
    taskLabel,
    { tag: `task-${taskLabel}` }
  );
}
