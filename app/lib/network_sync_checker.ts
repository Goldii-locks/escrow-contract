import { server, RPC_URL, NETWORK_PASSPHRASE } from "./contract";

/**
 * Interface representing the status result of a network synchronization check.
 */
export interface NetworkSyncStatus {
  isSynced: boolean;
  ledgerSequence?: number;
  latestLedgerCloseTime?: number;
  networkPassphrase?: string;
  error?: string;
}

/**
 * Options for configuring network_sync_checker execution.
 */
export interface NetworkSyncOptions {
  /** Optional callback or loader controller to toggle spinner/overlay state */
  onLoadingChange?: (isLoading: boolean) => void;
  /** Maximum timeout in milliseconds for the RPC sync check */
  timeoutMs?: number;
}

/**
 * State container for managing active spinner/loader ref counts cleanly.
 * Time Complexity: O(1) per state transition.
 * Space Complexity: O(1) auxiliary memory footprint.
 */
class SpinnerStateManager {
  private activeOperationsCount = 0;

  /**
   * Increments active operation count and triggers loading state if transitioning from 0 to 1.
   */
  startOperation(onLoadingChange?: (isLoading: boolean) => void): void {
    this.activeOperationsCount++;
    if (this.activeOperationsCount === 1 && onLoadingChange) {
      onLoadingChange(true);
    }
  }

  /**
   * Decrements active operation count and triggers idle state when active operations reach 0.
   */
  endOperation(onLoadingChange?: (isLoading: boolean) => void): void {
    this.activeOperationsCount = Math.max(0, this.activeOperationsCount - 1);
    if (this.activeOperationsCount === 0 && onLoadingChange) {
      onLoadingChange(false);
    }
  }

  /**
   * Returns current count of running sync operations.
   */
  getActiveCount(): number {
    return this.activeOperationsCount;
  }
}

export const spinnerManager = new SpinnerStateManager();

/**
 * Performs active network sync validation while triggering loader spinner states.
 * Guarantees spinner toggle cleanup via try...finally block.
 *
 * Time Complexity: O(1) overhead + RPC latency.
 * Space Complexity: O(1) auxiliary memory.
 *
 * @param options - Configuration including loading state listener
 * @returns Promise resolving to NetworkSyncStatus
 */
export async function checkNetworkSync(
  options: NetworkSyncOptions = {}
): Promise<NetworkSyncStatus> {
  const { onLoadingChange, timeoutMs = 10000 } = options;

  // Trigger loading spinner ON execution start
  spinnerManager.startOperation(onLoadingChange);

  try {
    const fetchPromise = (async (): Promise<NetworkSyncStatus> => {
      const health = await server.getHealth();
      if (health.status !== "healthy") {
        return {
          isSynced: false,
          error: `RPC node reported unhealthy status: ${health.status}`,
        };
      }

      const latestLedger = await server.getLatestLedger();
      return {
        isSynced: true,
        ledgerSequence: latestLedger.sequence,
        networkPassphrase: NETWORK_PASSPHRASE,
      };
    })();

    const timeoutPromise = new Promise<NetworkSyncStatus>((_, reject) => {
      setTimeout(() => {
        reject(new Error(`Network sync check timed out after ${timeoutMs}ms`));
      }, timeoutMs);
    });

    const result = await Promise.race([fetchPromise, timeoutPromise]);
    return result;
  } catch (err: any) {
    return {
      isSynced: false,
      error: err?.message || "Unknown network sync validation failure",
    };
  } finally {
    // Trigger loading spinner OFF on execution end (guaranteed cleanup)
    spinnerManager.endOperation(onLoadingChange);
  }
}
