import React from 'react'
import ReactDOM from 'react-dom/client'
import { ConfigProvider, theme } from 'antd'
import zhCN from 'antd/locale/zh_CN'
import App from './App'
import './App.css'

ReactDOM.createRoot(document.getElementById('root')).render(
  <ConfigProvider
    locale={zhCN}
    theme={{
      algorithm: theme.darkAlgorithm,
      token: {
        colorPrimary: '#2563eb',
        borderRadius: 10,
        colorBgContainer: '#1e293b',
        colorBgElevated: '#111827',
      },
    }}
  >
    <App />
  </ConfigProvider>
)
