// The entry point a React Native app is scaffolded with: a `.js` file that
// holds JSX (carrick#803).
import { useEffect, useState } from 'react';
import { Text, View } from 'react-native';

export default function App() {
  const [profile, setProfile] = useState(null);

  useEffect(() => {
    fetch('/api/profile')
      .then((res) => res.json())
      .then(setProfile);
  }, []);

  const save = async () => {
    await fetch('/api/profile', {
      method: 'PUT',
      body: JSON.stringify(profile),
    });
  };

  return (
    <View style={{ flex: 1 }}>
      <Text onPress={save}>{profile ? profile.name : 'loading'}</Text>
    </View>
  );
}
