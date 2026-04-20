import React, { createContext, useContext, useState, useEffect, useCallback } from 'react';
import type { ApiConfig } from '../api';
import type { Conversation, OnboardingStatus, Account } from '../types';
import { listConversations, listOnboarding, listAccounts } from '../api';

const getDefaultBaseUrl = () => {
  if (typeof window === 'undefined') return '';
  const envBase = import.meta.env.VITE_API_BASE as string | undefined;
  if (envBase) return envBase;
  const { hostname, port, protocol } = window.location;
  const isLocalHost = hostname === 'localhost' || hostname === '127.0.0.1';
  const isVitePort = port === '5173' || port === '5174' || port === '4173';
  if (isLocalHost && isVitePort) return `${protocol}//${hostname}:3000`;
  return window.location.origin;
};

const DEFAULT_CONFIG: ApiConfig = {
  baseUrl: getDefaultBaseUrl(),
  token: '',
};

const loadConfig = (): ApiConfig => {
  if (typeof window === 'undefined') return DEFAULT_CONFIG;
  const stored = window.localStorage.getItem('jossie_api');
  if (!stored) return DEFAULT_CONFIG;
  try {
    const parsed = JSON.parse(stored) as ApiConfig;
    return { ...DEFAULT_CONFIG, ...parsed };
  } catch {
    return DEFAULT_CONFIG;
  }
};

const persistConfig = (config: ApiConfig) => {
  if (typeof window === 'undefined') return;
  window.localStorage.setItem('jossie_api', JSON.stringify(config));
};

export type ActivityItem = {
  id: string;
  conversationId?: string;
  label: string;
  detail: string;
  at: string;
  tone?: 'normal' | 'success' | 'warn';
};

interface AppContextType {
  apiConfig: ApiConfig;
  setApiConfig: (config: ApiConfig) => void;
  conversations: Conversation[];
  setConversations: React.Dispatch<React.SetStateAction<Conversation[]>>;
  activeConversationId: string | null;
  setActiveConversationId: (id: string | null) => void;
  onboarding: OnboardingStatus[];
  setOnboarding: React.Dispatch<React.SetStateAction<OnboardingStatus[]>>;
  accounts: Account[];
  setAccounts: React.Dispatch<React.SetStateAction<Account[]>>;
  activity: ActivityItem[];
  setActivity: React.Dispatch<React.SetStateAction<ActivityItem[]>>;
  addActivity: (item: Omit<ActivityItem, 'id' | 'at'>) => void;
  statusMessage: string | null;
  setStatusMessage: (msg: string | null) => void;
  canConnect: boolean;
  refreshConversations: () => Promise<void>;
  refreshOnboarding: () => Promise<void>;
  refreshAccounts: () => Promise<void>;
}

const AppContext = createContext<AppContextType | undefined>(undefined);

export const AppProvider: React.FC<{ children: React.ReactNode }> = ({ children }) => {
  const [apiConfig, setApiConfigState] = useState<ApiConfig>(loadConfig);
  const [conversations, setConversations] = useState<Conversation[]>([]);
  const [activeConversationId, setActiveConversationId] = useState<string | null>(null);
  const [onboarding, setOnboarding] = useState<OnboardingStatus[]>([]);
  const [accounts, setAccounts] = useState<Account[]>([]);
  const [activity, setActivity] = useState<ActivityItem[]>([]);
  const [statusMessage, setStatusMessage] = useState<string | null>(null);

  const canConnect = Boolean(apiConfig.baseUrl && apiConfig.token);

  const setApiConfig = (config: ApiConfig) => {
    setApiConfigState(config);
    persistConfig(config);
  };

  const addActivity = useCallback((item: Omit<ActivityItem, 'id' | 'at'>) => {
    setActivity((prev) => [
      {
        id: `activity-${Math.random().toString(36).slice(2, 10)}`,
        at: new Date().toLocaleTimeString(),
        ...item,
      },
      ...prev,
    ].slice(0, 80));
  }, []);

  const refreshConversations = useCallback(async () => {
    if (!canConnect) return;
    try {
      const data = await listConversations(apiConfig);
      setConversations(data);
    } catch (error) {
      console.error('Failed to refresh conversations:', error);
    }
  }, [apiConfig, canConnect]);

  const refreshOnboarding = useCallback(async () => {
    if (!canConnect) return;
    try {
      const data = await listOnboarding(apiConfig);
      setOnboarding(data);
    } catch (error) {
      console.error('Failed to refresh onboarding:', error);
    }
  }, [apiConfig, canConnect]);

  const refreshAccounts = useCallback(async () => {
    if (!canConnect) return;
    try {
      const data = await listAccounts(apiConfig);
      setAccounts(data);
    } catch (error) {
      console.error('Failed to refresh accounts:', error);
    }
  }, [apiConfig, canConnect]);

  useEffect(() => {
    if (canConnect) {
      refreshConversations();
    }
  }, [apiConfig.baseUrl, apiConfig.token, canConnect, refreshConversations]);

  const value: AppContextType = {
    apiConfig,
    setApiConfig,
    conversations,
    setConversations,
    activeConversationId,
    setActiveConversationId,
    onboarding,
    setOnboarding,
    accounts,
    setAccounts,
    activity,
    setActivity,
    addActivity,
    statusMessage,
    setStatusMessage,
    canConnect,
    refreshConversations,
    refreshOnboarding,
    refreshAccounts,
  };

  return <AppContext.Provider value={value}>{children}</AppContext.Provider>;
};

export const useAppContext = () => {
  const context = useContext(AppContext);
  if (context === undefined) {
    throw new Error('useAppContext must be used within an AppProvider');
  }
  return context;
};
